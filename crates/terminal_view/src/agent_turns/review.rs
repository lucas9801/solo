//! SOLO: the view of what an agent changed: its pending changes (each file
//! against what the user last accepted of it), with hunks to keep or reject
//! and to comment on for the agent; what a turn changed, read only; and the
//! turns to roll back (G59).

use std::any::{Any, TypeId};
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use anyhow::Context as _;
use buffer_diff::{BufferDiff, BufferDiffEvent, DiffHunkStatus};
use collections::HashSet;
use editor::display_map::{BlockPlacement, BlockProperties, BlockStyle, CustomBlockId};
use editor::{
    DiffHunkRenderer, Editor, EditorEvent, MultiBuffer, PathKey, multibuffer_context_lines,
};
use futures::FutureExt as _;
use futures::future::{BoxFuture, Shared};
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, EntityId, EventEmitter, FocusHandle,
    Focusable, Pixels, Subscription, Task, WeakEntity, Window,
};
use language::{
    Buffer, BufferSnapshot, Capability, OffsetRangeExt as _, Point, ToOffset as _, ToPoint as _,
};
use project::{Project, ProjectPath, RemoveOptions};
use ui::{CommonAnimationExt as _, Divider, Tooltip, prelude::*};
use util::ResultExt as _;
use util::i18n::{is_chinese, tr};
use util::rel_path::RelPath;
use workspace::{
    Item, ItemNavHistory, Workspace,
    item::{ItemEvent, TabContentParams},
    searchable::SearchableItemHandle,
};

use super::diagnostics::{TurnDiagnostic, TurnDiagnostics};
use super::pending::{self, PendingFile, Rejection, ReviewComment, Version, normalized_text};
use super::snapshot::{
    ChangedFile, FileChange, MAX_UNTRACKED_FILE_BYTES, PartlyRestored, SnapshotStore, TreeId,
    TreePin, WorkTreeChanged, changed_large_files, linked_ancestor,
};
use super::{AgentTurn, AgentTurns, locked_files};
use crate::TerminalView;

/// What the view shows.
#[derive(Clone)]
enum Mode {
    /// SOLO: The pending changes: each file the agent changed against what
    /// the user last accepted of it (G59).
    Pending,
    /// SOLO: What a turn changed, from before it to its end, read only.
    Turn(usize),
    /// What rolling back to before a turn would discard, to confirm it.
    ConfirmRollback(usize),
    /// Rolled back to before a turn, which can be undone.
    RolledBack {
        number: usize,
        store: Arc<SnapshotStore>,
        undo: TreeId,
        /// SOLO: Keeps `undo`'s objects (G51).
        _undo_pin: Arc<TreePin>,
        file_count: usize,
        /// SOLO: Rolling back failed partway: some files are rolled back
        /// and some aren't, until it's undone (R3 M2).
        stopped_partway: bool,
        /// SOLO: The pending files before, to put back when it's undone.
        previous: Option<BTreeMap<String, PendingFile>>,
    },
}

/// SOLO: Which pending changes are shown, when they're from more than one
/// turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TurnFilter {
    All,
    Turn(usize),
    /// The turn under way: its files are locked.
    Running,
}

/// A changed file, loaded to show its diff.
pub(crate) struct LoadedFile {
    pub path: String,
    pub abs_path: PathBuf,
    /// The contents compared with, or `None` if there was no file.
    pub base: Option<String>,
    pub buffer: Entity<Buffer>,
    /// SOLO: The pending file shown, in the pending changes.
    pub pending: Option<PendingFile>,
    /// SOLO: Each turn's versions of the file since it was accepted, newest
    /// first, to tell which turn each hunk is from.
    pub turn_versions: Arc<Vec<(usize, Version, Version)>>,
}

/// SOLO: A file whose hunks were rejected: the pending file as the review
/// left it, and what was rejected, for the agent.
type RejectedFile = (Option<(String, Option<PendingFile>, bool)>, Vec<Rejection>);

/// SOLO: Why a file wasn't rejected: it changed on disk.
fn changed_on_disk() -> &'static str {
    tr(
        "it changed on disk since the page showed it; try again",
        "页面显示之后它在磁盘上又改过了，请再试一次",
    )
}

/// SOLO: The contents of `objects` of `store` that it has, each once.
fn accepted_on_disk(store: &SnapshotStore, objects: Vec<String>) -> Vec<Vec<u8>> {
    let ids = store.object_ids(&objects).log_err().unwrap_or_default();
    let mut seen = HashSet::default();
    let present: Vec<String> = objects
        .into_iter()
        .zip(ids)
        .filter_map(|(object, id)| seen.insert(id?).then_some(object))
        .collect();
    store
        .objects_contents(&present)
        .log_err()
        .unwrap_or_default()
        .into_iter()
        .flatten()
        .collect()
}

/// SOLO: Where `edits` of a text (sorted, by offset in it) are in the text
/// they make: each one's offsets there.
fn edits_after(edits: &[(Range<usize>, String)]) -> Vec<Range<usize>> {
    let mut delta: isize = 0;
    edits
        .iter()
        .map(|(range, text)| {
            let start = (range.start as isize + delta).max(0) as usize;
            delta += text.len() as isize - range.len() as isize;
            start..start + text.len()
        })
        .collect()
}

/// SOLO: The rows of `text` each of `ranges` (offsets) has, as
/// [`point_rows`] tells them.
fn offset_edits_rows(text: &str, ranges: &[Range<usize>]) -> Vec<Range<u32>> {
    let point = |offset: usize| -> Point {
        let offset = offset.min(text.len());
        let before = text.get(..offset).unwrap_or_default();
        let row = before.matches('\n').count() as u32;
        let column = before
            .rfind('\n')
            .map_or(before.len(), |ix| before.len() - ix - 1);
        Point::new(row, column as u32)
    };
    ranges
        .iter()
        .map(|range| point_rows(&(point(range.start)..point(range.end))))
        .collect()
}

/// A hunk of the diff shown, in its file.
struct Hunk {
    buffer_range: Range<language::Anchor>,
    base_range: Range<usize>,
}

struct ReviewFile {
    /// SOLO: Relative to the work tree, with `/` separators.
    path: String,
    abs_path: PathBuf,
    path_key: PathKey,
    buffer: Entity<Buffer>,
    diff: Entity<BufferDiff>,
    /// What the file is compared with: what the user accepted of it (or its
    /// contents before the turn shown), with the hunks kept since. `None`
    /// if there was no file.
    base: Option<Arc<str>>,
    /// SOLO: The pending file shown (G59).
    pending: Option<PendingFile>,
    turn_versions: Arc<Vec<(usize, Version, Version)>>,
    /// SOLO: Which turns the hunks are from, by rows of the buffer.
    edit_turns: Vec<(Range<u32>, BTreeSet<usize>)>,
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
    /// SOLO: Large untracked files that changed, which no snapshot has: not
    /// shown, and not rolled back (G51).
    large_files: Vec<String>,
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
    /// SOLO: The work tree of the changes shown: files reached through a
    /// link in it (a junction, say) may be outside it, and aren't deleted.
    work_tree: Option<PathBuf>,
    /// SOLO: Moving rejected new files to the recycle bin.
    trash_task: Task<()>,
    /// SOLO: The work tree the rollback confirmation shows; rolling back
    /// stops if it changed since.
    confirmed_tree: Option<TreeId>,
    /// SOLO: Which turns' pending changes are shown (G59).
    filter: TurnFilter,
    /// SOLO: The pending files the agent is changing now: not shown.
    locked: Vec<String>,
    /// SOLO: The hunks rejected since the last message to the agent, when
    /// there are no pending changes to keep them with (else they're kept
    /// there: see [`AgentTurns::unsent`]).
    rejected: Vec<Rejection>,
    /// SOLO: Review comments not sent to the agent, from before this page
    /// (it was closed, or Solo restarted), not in the editor yet: they're
    /// put there once their lines show.
    carried: Vec<ReviewComment>,
    /// SOLO: Where each of `carried` is now, once that's looked for: its
    /// file's buffer and rows.
    carried_rows: Vec<Option<(language::BufferId, Range<u32>)>>,
    carried_search: Task<()>,
    /// SOLO: Whether comments from before are being put in the editor.
    placing: bool,
    /// What a test does once a rejected file is written, before the rest.
    #[cfg(test)]
    after_write: Option<Box<dyn FnOnce(&mut Self, &mut Context<Self>)>>,
    /// SOLO: The rejection of each file under way, which the next one of the
    /// file waits for: it starts from what that one left.
    rejecting: collections::HashMap<language::BufferId, Shared<BoxFuture<'static, ()>>>,
    /// SOLO: The window it's in, to show the comments from before in.
    window: gpui::AnyWindowHandle,
    /// SOLO: What the pending changes shown were loaded from, to reload
    /// when that changes.
    shown: Option<ShownState>,
    /// SOLO: Whether it's in a pane, counted as open: earlier turns'
    /// changes aren't accepted while it is (G59).
    counted_open: std::cell::Cell<bool>,
    /// SOLO: Whether it's the page of the pending changes, not of a turn.
    pending_page: bool,
    /// SOLO: The store of the pending changes shown.
    store_dir: Option<PathBuf>,
}

/// SOLO: What the pending changes shown were loaded from.
#[derive(Clone, PartialEq)]
struct ShownState {
    revision: usize,
    turn_count: usize,
    /// The turns under way or ending: number, whether their first
    /// snapshot is taken, and how many files they reported editing.
    active: Vec<(usize, bool, usize)>,
}

impl AgentTurnReview {
    /// Shows the pending changes of `terminal_view`'s agent, in the view
    /// already showing its changes if there's one.
    pub(crate) fn open(
        terminal_view: Entity<TerminalView>,
        workspace: Entity<Workspace>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let terminal_id = terminal_view.entity_id();
        AgentTurns::global(cx).update(cx, |turns, cx| turns.dismiss_offer(terminal_id, cx));
        workspace.update(cx, |workspace, cx| {
            let existing = workspace.items_of_type::<AgentTurnReview>(cx).find(|review| {
                let review = review.read(cx);
                review.terminal_id == terminal_id && review.pending_page
            });
            if let Some(existing) = existing {
                workspace.activate_item(&existing, true, true, window, cx);
                existing.update(cx, |review, cx| review.load(window, cx));
                return;
            }
            let project = workspace.project().clone();
            let review =
                cx.new(|cx| AgentTurnReview::new(terminal_view, project, true, window, cx));
            workspace.add_item_to_active_pane(Box::new(review), None, true, window, cx);
        });
    }

    /// SOLO: Shows a turn of `terminal_view`'s agent (what it changed, or
    /// what rolling back to before it discards) in a page of its own, so
    /// the pending changes' page keeps its comments (G59).
    fn open_turn(&self, mode: Mode, window: &mut Window, cx: &mut Context<Self>) {
        let Some(terminal_view) = self.terminal_view.upgrade() else {
            return;
        };
        let Some(workspace) = terminal_view.read(cx).workspace.upgrade() else {
            return;
        };
        let terminal_id = self.terminal_id;
        window.defer(cx, move |window, cx| {
            workspace.update(cx, |workspace, cx| {
                let existing = workspace.items_of_type::<AgentTurnReview>(cx).find(|review| {
                    let review = review.read(cx);
                    review.terminal_id == terminal_id && !review.pending_page
                });
                if let Some(existing) = existing {
                    workspace.activate_item(&existing, true, true, window, cx);
                    existing.update(cx, |review, cx| review.set_mode(mode, window, cx));
                    return;
                }
                let project = workspace.project().clone();
                let review = cx.new(|cx| {
                    let mut review = AgentTurnReview::new(terminal_view, project, false, window, cx);
                    review.pending_page = false;
                    review.set_mode(mode, window, cx);
                    review
                });
                workspace.add_item_to_active_pane(Box::new(review), None, true, window, cx);
            });
        });
    }

    /// SOLO: Back from a turn's page to the pending changes' page, closing
    /// the turn's.
    fn back_to_pending(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pending_page {
            self.set_mode(Mode::Pending, window, cx);
            return;
        }
        let Some(terminal_view) = self.terminal_view.upgrade() else {
            return;
        };
        let Some(workspace) = terminal_view.read(cx).workspace.upgrade() else {
            return;
        };
        let item_id = cx.entity_id();
        window.defer(cx, move |window, cx| {
            AgentTurnReview::open(terminal_view, workspace.clone(), window, cx);
            let panes = workspace.read(cx).panes().to_vec();
            for pane in panes {
                pane.update(cx, |pane, cx| {
                    pane.close_item_by_id(item_id, workspace::SaveIntent::Skip, window, cx)
                        .detach_and_log_err(cx)
                });
            }
        });
    }

    fn new(
        terminal_view: Entity<TerminalView>,
        project: Entity<Project>,
        load: bool,
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
        let terminal_id = terminal_view.entity_id();
        let turns = AgentTurns::global(cx);
        cx.on_release(move |this, cx| {
            if this.counted_open.get() {
                close_review(terminal_id, cx);
            }
        })
        .detach();
        cx.observe_in(&turns, window, |this, _, window, cx| {
            this.turns_changed(window, cx);
            cx.notify();
        })
        .detach();
        let renderer: Arc<dyn DiffHunkRenderer> = Arc::new(TurnHunkRenderer {
            review: cx.weak_entity(),
        });
        editor.update(cx, |editor, cx| {
            editor.set_diff_hunk_renderer(Some(renderer), cx)
        });
        cx.subscribe(&editor, |this, _, event: &EditorEvent, cx| {
            if let EditorEvent::ReviewCommentsChanged { total_count } = event {
                this.comment_count = *total_count;
                this.keep_comments(cx);
                cx.notify();
            }
            cx.emit(event.clone())
        })
        .detach();

        let mut this = Self {
            terminal_id,
            terminal_view: terminal_view.downgrade(),
            project,
            multibuffer,
            editor,
            focus_handle: cx.focus_handle(),
            files: Vec::new(),
            skipped_files: 0,
            large_files: Vec::new(),
            mode: Mode::Pending,
            loading: false,
            status: None,
            show_history: false,
            comment_count: 0,
            scope_blocks: HashSet::default(),
            load_task: Task::ready(()),
            action_task: Task::ready(()),
            work_tree: None,
            trash_task: Task::ready(()),
            confirmed_tree: None,
            filter: TurnFilter::All,
            locked: Vec::new(),
            rejected: Vec::new(),
            // SOLO: the comments not sent from before, on the pending
            // changes' page.
            carried: if load {
                AgentTurns::try_global(cx)
                    .map(|turns| turns.read(cx).unsent(terminal_id).comments)
                    .unwrap_or_default()
            } else {
                Vec::new()
            },
            carried_rows: Vec::new(),
            carried_search: Task::ready(()),
            placing: false,
            #[cfg(test)]
            after_write: None,
            rejecting: collections::HashMap::default(),
            window: window.window_handle(),
            shown: None,
            counted_open: std::cell::Cell::new(false),
            pending_page: true,
            store_dir: None,
        };
        if load {
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
        // SOLO: a turn's page shows other files than the pending changes.
        let reset = !matches!((&self.mode, &mode), (Mode::Pending, Mode::Pending));
        self.mode = mode;
        self.status = None;
        self.show_history = false;
        if reset {
            self.set_files(Vec::new(), 0, cx);
            self.shown = None;
            self.filter = TurnFilter::All;
        }
        let comments = matches!(self.mode, Mode::Pending);
        self.editor.update(cx, |editor, cx| {
            editor.set_show_diff_review_button(comments, cx)
        });
        self.load(window, cx);
        cx.emit(EditorEvent::TitleChanged);
        cx.notify();
    }

    /// SOLO: What the pending changes shown depend on, as it is now.
    fn shown_state(&self, cx: &App) -> Option<ShownState> {
        let turns = AgentTurns::try_global(cx)?.read(cx);
        Some(ShownState {
            revision: turns
                .pending_changes(self.terminal_id)
                .map_or(0, |changes| changes.revision),
            turn_count: turns.turns(self.terminal_id).count(),
            active: turns
                .active_turns(self.terminal_id)
                .iter()
                .map(|turn| {
                    (
                        turn.number,
                        turn.before.is_some(),
                        turn.edited.as_ref().map_or(0, Vec::len),
                    )
                })
                .collect(),
        })
    }

    /// SOLO: Reloads the pending changes when what they're loaded from
    /// changed: a turn started or ended, or the agent edited a file.
    fn turns_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.mode, Mode::Pending) {
            return;
        }
        let state = self.shown_state(cx);
        if state.is_some() && state != self.shown {
            self.load(window, cx);
        }
    }

    /// Loads the files the mode shows.
    fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let number = match &self.mode {
            Mode::Pending => {
                self.load_pending(window, cx);
                return;
            }
            Mode::Turn(number) | Mode::ConfirmRollback(number) => *number,
            Mode::RolledBack { .. } => {
                self.large_files.clear();
                self.set_files(Vec::new(), 0, cx);
                return;
            }
        };
        let Some(turn) = self.turn(number, cx) else {
            self.large_files.clear();
            self.set_files(Vec::new(), 0, cx);
            self.status = Some(tr("This turn is no longer available.", "这一轮已不可用。").into());
            return;
        };
        self.work_tree = Some(turn.store.work_tree().to_path_buf());
        let confirming = matches!(self.mode, Mode::ConfirmRollback(_));
        self.confirmed_tree = None;
        self.large_files = if confirming {
            Vec::new()
        } else {
            turn.large_files.clone()
        };
        self.loading = true;
        let project = self.project.clone();
        self.load_task = cx.spawn_in(window, async move |this, cx| {
            let store = turn.store.clone();
            let before = turn.before.clone();
            let loaded = cx
                .background_spawn(async move {
                    // Rolling back discards everything since the turn started,
                    // not only what the turn changed.
                    let (after, changed, large_files) = if confirming {
                        let (now, now_large) = store.snapshot_with_large_files()?;
                        let changed = store.changed_files(&before, &now)?;
                        // SOLO: and the large files it can't restore (G51).
                        let large_files = changed_large_files(&turn.before_large, &now_large);
                        (now, changed, Some(large_files))
                    } else {
                        (turn.after.clone(), turn.changed.clone(), None)
                    };
                    let shown = confirming.then(|| after.clone());
                    anyhow::Ok((
                        load_bases(&store, &before, &after, changed),
                        shown,
                        large_files,
                    ))
                })
                .await;
            let (bases, mut skipped) = match loaded {
                Ok((loaded, shown, large_files)) => {
                    this.update(cx, |this, _| {
                        this.confirmed_tree = shown;
                        if let Some(large_files) = large_files {
                            this.large_files = large_files;
                        }
                    })
                    .ok();
                    loaded
                }
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
            let languages = project.read_with(cx, |project, _| project.languages().clone());
            let mut files = Vec::new();
            for (path, abs_path, base, after) in bases {
                // SOLO: a turn's own page shows the file as the turn left it,
                // not as it is now (G59).
                let buffer = if confirming {
                    project
                        .update(cx, |project, cx| project.open_local_buffer(&abs_path, cx))
                        .await
                } else {
                    let language = languages
                        .load_language_for_file_path(&abs_path)
                        .await
                        .ok();
                    let text = after.unwrap_or_default();
                    let worktree_id = project.read_with(cx, |project, cx| {
                        project
                            .find_worktree(&abs_path, cx)
                            .map(|(worktree, _)| worktree.read(cx).id())
                    });
                    let file = RelPath::from_unix_str(&path).ok().map(|relative| {
                        Arc::new(TurnFile {
                            path: relative.into_arc(),
                            abs_path: abs_path.clone(),
                            worktree_id: worktree_id
                                .unwrap_or_else(|| project::WorktreeId::from_usize(usize::MAX)),
                            name: abs_path
                                .file_name()
                                .map(|name| name.to_string_lossy().into_owned())
                                .unwrap_or_else(|| path.clone()),
                        }) as Arc<dyn language::File>
                    });
                    Ok(cx.new(|cx| {
                        let mut buffer = Buffer::local(text, cx);
                        // SOLO: named as the file it was (G59).
                        if let Some(file) = file {
                            buffer.file_updated(file, cx);
                        }
                        buffer.set_language(language, cx);
                        buffer
                    }))
                };
                match buffer {
                    Ok(buffer) => files.push(LoadedFile {
                        path,
                        abs_path,
                        base,
                        buffer,
                        pending: None,
                        turn_versions: Arc::default(),
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

    /// SOLO: Loads the pending changes: each file the agent isn't changing
    /// now, against what the user accepted of it, with what the user did
    /// since taken in (see [`pending::reconcile`]) (G59).
    fn load_pending(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.large_files.clear();
        self.shown = self.shown_state(cx);
        let Some(turns) = AgentTurns::try_global(cx).cloned() else {
            return;
        };
        let (changes, active, history) = {
            let turns = turns.read(cx);
            (
                turns.pending_changes(self.terminal_id).cloned(),
                turns.active_turns(self.terminal_id),
                turns
                    .turns(self.terminal_id)
                    .rev()
                    .cloned()
                    .collect::<Vec<_>>(),
            )
        };
        let Some(changes) = changes else {
            self.locked.clear();
            self.set_files(Vec::new(), 0, cx);
            return;
        };
        self.work_tree = Some(changes.store.work_tree().to_path_buf());
        self.store_dir = Some(changes.store.store_dir().to_path_buf());
        self.loading = true;
        let project = self.project.clone();
        self.load_task = cx.spawn_in(window, async move |this, cx| {
            let store = changes.store.clone();
            let files = changes.files.clone();
            let loaded = cx
                .background_spawn(async move {
                    let locked = locked_files(&store, files.keys(), &active);
                    // Each turn's versions of the files, to tell which turn
                    // each hunk is from; and what's committed.
                    let history: Vec<&AgentTurn> = history
                        .iter()
                        .filter(|turn| turn.store.store_dir() == store.store_dir())
                        .collect();
                    let mut objects = Vec::new();
                    let mut wanted = Vec::new();
                    for (path, file) in &files {
                        if locked.contains(path) {
                            continue;
                        }
                        for turn in &history {
                            if turn.number < file.first_turn {
                                continue;
                            }
                            objects.push(format!("{}:{path}", turn.before));
                            objects.push(format!("{}:{path}", turn.after));
                            wanted.push((path.clone(), turn.number));
                        }
                    }
                    let mut versions = pending::load_versions(&store, &objects)
                        .log_err()
                        .unwrap_or_default()
                        .into_iter();
                    let mut turn_versions: BTreeMap<String, Vec<(usize, Version, Version)>> =
                        BTreeMap::new();
                    for (path, number) in wanted {
                        let (Some(before), Some(after)) = (versions.next(), versions.next())
                        else {
                            break;
                        };
                        turn_versions
                            .entry(path)
                            .or_default()
                            .push((number, before, after));
                    }
                    let committed: BTreeMap<String, Version> = if store.is_repository() {
                        let paths: Vec<&String> = files.keys().collect();
                        let objects: Vec<String> =
                            paths.iter().map(|path| format!("HEAD:{path}")).collect();
                        let versions = pending::load_versions(&store, &objects)
                            .log_err()
                            .unwrap_or_default();
                        paths.into_iter().cloned().zip(versions).collect()
                    } else {
                        BTreeMap::new()
                    };
                    (locked, turn_versions, committed)
                })
                .await;
            let (locked, mut turn_versions, committed) = loaded;
            let mut loaded_files = Vec::new();
            let mut skipped = 0;
            for (path, file) in &changes.files {
                if locked.contains(path) {
                    continue;
                }
                if matches!(file.accepted, Version::Binary(_))
                    || matches!(file.left, Version::Binary(_))
                {
                    skipped += 1;
                    continue;
                }
                let abs_path = changes.store.abs_path(path);
                let buffer = project
                    .update(cx, |project, cx| project.open_local_buffer(&abs_path, cx))
                    .await;
                let buffer = match buffer {
                    Ok(buffer) => buffer,
                    Err(error) => {
                        log::warn!("can't show changes to {abs_path:?}: {error:#}");
                        skipped += 1;
                        continue;
                    }
                };
                // What the user did since the file was last left.
                let current = cx.update(|_, cx| buffer_version(&buffer, cx)).ok().flatten();
                let mut file = file.clone();
                if let Some(current) = current
                    && current != file.left
                {
                    let accepted = pending::rebase(&file.accepted, &file.left, &current);
                    file.accepted = pending::keep_committed_version(
                        &accepted,
                        &current,
                        committed.get(path),
                    );
                    file.left = current;
                }
                loaded_files.push(LoadedFile {
                    path: path.clone(),
                    abs_path,
                    base: match &file.accepted {
                        Version::Text(text) => Some(text.to_string()),
                        _ => None,
                    },
                    buffer,
                    turn_versions: Arc::new({
                        let mut versions = turn_versions.remove(path).unwrap_or_default();
                        // Before this session's turns: what changed while Solo
                        // was closed.
                        if let Some((from, to)) = &file.while_closed {
                            versions.push((pending::WHILE_CLOSED, from.clone(), to.clone()));
                        }
                        versions
                    }),
                    pending: Some(file),
                });
            }
            this.update(cx, |this, cx| {
                this.loading = false;
                this.locked = locked.into_iter().collect();
                // What the user did is taken in.
                let updates: Vec<(String, Option<PendingFile>, bool)> = loaded_files
                    .iter()
                    .filter_map(|loaded| {
                        let pending = loaded.pending.clone()?;
                        let original = changes.files.get(&loaded.path)?;
                        (*original != pending).then(|| {
                            let has_changes = pending.accepted != pending.left;
                            (loaded.path.clone(), Some(pending), has_changes)
                        })
                    })
                    .collect();
                if !updates.is_empty() {
                    this.model_update(updates, cx);
                }
                this.set_files(loaded_files, skipped, cx);
                if this.filter != TurnFilter::All
                    && !this.turn_sources(cx).contains(&this.filter)
                {
                    this.filter = TurnFilter::All;
                }
                // SOLO: where the comments from before are now.
                this.find_carried_comments(cx);
            })
            .ok();
        });
        cx.notify();
    }

    /// SOLO: Shows `files`, keeping the files already shown that are among
    /// them (with the comments on them).
    pub(crate) fn set_files(
        &mut self,
        files: Vec<LoadedFile>,
        skipped_files: usize,
        cx: &mut Context<Self>,
    ) {
        self.skipped_files = skipped_files;
        let wanted: Vec<PathBuf> = files.iter().map(|file| file.abs_path.clone()).collect();
        let removed: Vec<PathKey> = self
            .files
            .iter()
            .filter(|file| !wanted.contains(&file.abs_path))
            .map(|file| file.path_key.clone())
            .collect();
        self.files.retain(|file| wanted.contains(&file.abs_path));
        if !removed.is_empty() {
            self.multibuffer.update(cx, |multibuffer, cx| {
                multibuffer.remove_excerpts_for_paths(removed, cx)
            });
        }
        for file in files {
            // SOLO: without a BOM too, which the buffer doesn't have: with
            // it, the first line always differs, and rejecting that would
            // write a second BOM (R3 L-G7).
            let base: Option<Arc<str>> = file.base.map(|base| normalized_text(base).into());
            if let Some(ix) = self
                .files
                .iter()
                .position(|shown| shown.abs_path == file.abs_path && shown.buffer == file.buffer)
            {
                let shown = &mut self.files[ix];
                shown.base = base;
                shown.pending = file.pending;
                shown.turn_versions = file.turn_versions;
                self.update_diff(ix, cx);
                continue;
            }
            if let Some(ix) = self
                .files
                .iter()
                .position(|shown| shown.abs_path == file.abs_path)
            {
                let path_key = self.files.remove(ix).path_key;
                self.multibuffer.update(cx, |multibuffer, cx| {
                    multibuffer.remove_excerpts_for_paths(vec![path_key], cx)
                });
            }
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
                        if let Some(ix) = this.file_index(buffer_id, cx)
                            && this.buffer_changed(ix, cx)
                        {
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
                path: file.path,
                abs_path: file.abs_path,
                path_key,
                buffer: file.buffer,
                diff,
                base,
                pending: file.pending,
                turn_versions: file.turn_versions,
                edit_turns: Vec::new(),
                diff_update: Task::ready(()).shared(),
                _subscriptions: subscriptions,
            });
            self.update_diff(self.files.len() - 1, cx);
        }
        cx.notify();
    }

    /// SOLO: Tells the pending changes what the page changed of them.
    fn model_update(&self, updates: Vec<(String, Option<PendingFile>, bool)>, cx: &mut App) {
        let Some(store_dir) = self.store_dir.clone() else {
            return;
        };
        let terminal_id = self.terminal_id;
        AgentTurns::global(cx).update(cx, |turns, cx| {
            turns.update_pending_files(terminal_id, &store_dir, updates, cx)
        });
    }

    fn file_index(&self, buffer_id: language::BufferId, cx: &App) -> Option<usize> {        self.files
            .iter()
            .position(|file| file.buffer.read(cx).remote_id() == buffer_id)
    }

    /// SOLO: A pending file changed in its buffer. While a turn is under
    /// way, a file it may be changing is locked; otherwise, what someone
    /// else changed that doesn't touch the pending hunks is accepted (G59).
    /// Whether the file is still shown.
    fn buffer_changed(&mut self, ix: usize, cx: &mut Context<Self>) -> bool {
        if !matches!(self.mode, Mode::Pending) {
            return true;
        }
        let Some(file) = self.files.get(ix) else {
            return false;
        };
        let Some(pending) = file.pending.clone() else {
            return true;
        };
        let Some(current) = buffer_version(&file.buffer, cx) else {
            return true;
        };
        if current == pending.left {
            return true;
        }
        let path = file.path.clone();
        let active = AgentTurns::try_global(cx)
            .map(|turns| turns.read(cx).active_turns(self.terminal_id))
            .unwrap_or_default();
        let abs_path = file.abs_path.clone();
        let changing = active.iter().any(|turn| match &turn.edited {
            Some(edited) => edited
                .iter()
                .any(|edited| super::is_same_path(edited, &abs_path)),
            None => true,
        });
        if changing {
            // Shown again when the turn ends.
            let path_key = file.path_key.clone();
            self.files.remove(ix);
            self.multibuffer.update(cx, |multibuffer, cx| {
                multibuffer.remove_excerpts_for_paths(vec![path_key], cx)
            });
            if !self.locked.contains(&path) {
                self.locked.push(path);
                self.locked.sort();
            }
            cx.notify();
            return false;
        }
        let accepted = pending::rebase(&pending.accepted, &pending.left, &current);
        let updated = PendingFile {
            accepted,
            left: current.clone(),
            ..pending
        };
        let file = &mut self.files[ix];
        file.base = match &updated.accepted {
            Version::Text(text) => Some(text.clone()),
            _ => None,
        };
        file.pending = Some(updated.clone());
        let has_changes = updated.accepted != current;
        self.model_update(vec![(path, Some(updated), has_changes)], cx);
        true
    }

    /// Diffs the file against its base again, after any update in progress;
    /// SOLO: then tells which turns its hunks are from.
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
                let Ok(Some((base, snapshot, turn_versions))) = this.update(cx, |this, cx| {
                    let buffer_id = buffer.read(cx).remote_id();
                    let file = &this.files[this.file_index(buffer_id, cx)?];
                    if file.diff != diff {
                        return None;
                    }
                    Some((
                        file.base.clone(),
                        buffer.read(cx).text_snapshot(),
                        file.turn_versions.clone(),
                    ))
                }) else {
                    return;
                };
                let text = snapshot.text();
                diff.update(cx, |diff, cx| diff.set_base_text(base.clone(), snapshot, cx))
                    .await;
                if turn_versions.is_empty() {
                    return;
                }
                let edit_turns = cx
                    .background_spawn(async move {
                        let versions: Vec<pending::TurnVersions> = turn_versions
                            .iter()
                            .filter_map(|(number, before, after)| {
                                Some(pending::TurnVersions {
                                    number: *number,
                                    before: before.text()?,
                                    after: after.text()?,
                                })
                            })
                            .collect();
                        pending::edit_turns(base.as_deref().unwrap_or(""), &text, &versions)
                    })
                    .await;
                this.update(cx, |this, cx| {
                    let buffer_id = buffer.read(cx).remote_id();
                    if let Some(ix) = this.file_index(buffer_id, cx) {
                        this.files[ix].edit_turns = edit_turns;
                        this.refresh_excerpts(ix, cx);
                    }
                })
                .ok();
            })
            .shared();
    }

    /// SOLO: Whether a hunk of the file at `ix` is among those shown, by
    /// the turn filtered by.
    fn in_filter(&self, ix: usize, range: &Range<language::Anchor>, cx: &App) -> bool {
        match self.filter {
            TurnFilter::All => true,
            TurnFilter::Running => false,
            TurnFilter::Turn(number) => {
                let snapshot = self.files[ix].buffer.read(cx).snapshot();
                self.hunk_turns(ix, &point_rows(&range.to_point(&snapshot)))
                    .contains(&number)
            }
        }
    }

    /// SOLO: The turns of a hunk of the file at `ix`, by its rows.
    fn hunk_turns(&self, ix: usize, rows: &Range<u32>) -> BTreeSet<usize> {
        self.files
            .get(ix)
            .map(|file| pending::turns_at(&file.edit_turns, rows))
            .unwrap_or_default()
    }

    /// SOLO: The turns the pending changes shown are from, with the turn
    /// under way when it locks files. A filter for each, when there's more
    /// than one.
    fn turn_sources(&self, cx: &App) -> Vec<TurnFilter> {
        if !matches!(self.mode, Mode::Pending) {
            return Vec::new();
        }
        let numbers: BTreeSet<usize> = self
            .files
            .iter()
            .flat_map(|file| file.edit_turns.iter().flat_map(|(_, turns)| turns.iter()))
            .copied()
            .collect();
        let running = !self.locked.is_empty()
            && AgentTurns::try_global(cx).is_some_and(|turns| {
                !turns.read(cx).active_turns(self.terminal_id).is_empty()
            });
        let mut sources: Vec<TurnFilter> = numbers.into_iter().map(TurnFilter::Turn).collect();
        if running {
            sources.push(TurnFilter::Running);
        }
        sources
    }

    /// SOLO: Whether the pending changes come from more than one place,
    /// so hunks say which turn they're from: hunks from before this session
    /// (no turn known) count as one.
    fn shows_turns(&self, cx: &App) -> bool {
        let earlier = self.files.iter().any(|file| {
            (file.pending.is_some() && file.turn_versions.is_empty())
                || file.edit_turns.iter().any(|(_, turns)| turns.is_empty())
        });
        self.turn_sources(cx).len() + usize::from(earlier) > 1
    }

    /// Shows the file's hunks, or nothing when it has none left; SOLO: only
    /// those of the turn filtered by (G59).
    fn refresh_excerpts(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(file) = self.files.get(ix) else {
            return;
        };
        let snapshot = file.buffer.read(cx).snapshot();
        let filter = self.filter;
        let ranges: Vec<Range<Point>> = file
            .diff
            .read(cx)
            .snapshot(cx)
            .hunks_intersecting_range(
                language::Anchor::min_max_range_for_buffer(snapshot.remote_id()),
                &snapshot,
            )
            .map(|hunk| hunk.buffer_range.to_point(&snapshot))
            .filter(|range| match filter {
                TurnFilter::All => true,
                TurnFilter::Running => false,
                TurnFilter::Turn(number) => self
                    .hunk_turns(ix, &point_rows(range))
                    .contains(&number),
            })
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
        // SOLO: the comments from before go where their lines now show.
        self.place_carried_comments(cx);
        cx.notify();
    }

    fn set_filter(&mut self, filter: TurnFilter, cx: &mut Context<Self>) {
        self.filter = filter;
        for ix in 0..self.files.len() {
            self.refresh_excerpts(ix, cx);
        }
        cx.notify();
    }

    /// Puts a line above each hunk naming the functions or types it's in,
    /// to tell what a change touches without scrolling up for it; SOLO: and
    /// which turns it's from, when the changes shown are from more than one
    /// (G59).
    fn update_scope_labels(&mut self, cx: &mut Context<Self>) {
        let context_lines = multibuffer_context_lines(cx);
        let snapshot = self.multibuffer.read(cx).snapshot(cx);
        let show_turns = self.shows_turns(cx);
        let mut labels: Vec<(editor::Anchor, Option<String>, Option<String>)> = Vec::new();
        for hunk in snapshot.diff_hunks() {
            let Some(ix) = self.file_index(hunk.buffer_id, cx) else {
                continue;
            };
            let buffer = self.files[ix].buffer.read(cx).snapshot();
            let range = hunk.buffer_range.to_point(&buffer);
            let scope = hunk_scope(&buffer, range.start, context_lines);
            let turns = show_turns
                .then(|| turns_label(&self.hunk_turns(ix, &point_rows(&range))))
                .flatten();
            if scope.is_some() || turns.is_some() {
                labels.push((hunk.multi_buffer_range.start, turns, scope));
            }
        }
        let old = std::mem::take(&mut self.scope_blocks);
        self.scope_blocks = self.editor.update(cx, |editor, cx| {
            editor.remove_blocks(old, None, cx);
            let blocks = labels.into_iter().map(|(anchor, turns, scope)| BlockProperties {
                placement: BlockPlacement::Above(anchor),
                height: Some(1),
                style: BlockStyle::Flex,
                render: Arc::new(move |cx| {
                    let font_size = cx
                        .editor_style
                        .text
                        .font_size
                        .to_pixels(cx.window.rem_size());
                    let colors = cx.app.theme().colors();
                    h_flex()
                        .h(cx.line_height)
                        .pl(cx.anchor_x)
                        .gap_2()
                        .overflow_hidden()
                        .when_some(turns.clone(), |this, turns| {
                            this.child(
                                h_flex()
                                    .flex_none()
                                    .px_1p5()
                                    .rounded_full()
                                    .border_1()
                                    .border_color(colors.border)
                                    .bg(colors.element_background)
                                    .child(Label::new(turns).size(LabelSize::XSmall)),
                            )
                        })
                        .when_some(scope.clone(), |this, scope| {
                            this.child(
                                div()
                                    .font(cx.editor_style.text.font())
                                    .text_size(font_size * 0.9)
                                    .text_color(colors.text_muted)
                                    .truncate()
                                    .child(scope),
                            )
                        })
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
                // SOLO: only the turn shown's, when filtered by turn: a hunk
                // of another can be in an excerpt's context (G59).
                if !self.in_filter(ix, &hunk.buffer_range, cx) {
                    continue;
                }
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

    /// SOLO: The path to tell the agent: relative to its folder when it's in
    /// there; `path` (relative to the work tree) when the folder isn't known.
    fn agent_path(&self, abs_path: &Path, path: &str, cx: &App) -> String {
        let directory = match &self.mode {
            Mode::Pending => AgentTurns::try_global(cx).and_then(|turns| {
                turns
                    .read(cx)
                    .pending_changes(self.terminal_id)
                    .map(|changes| changes.directory.clone())
            }),
            Mode::Turn(number) | Mode::ConfirmRollback(number) => {
                self.turn(*number, cx).map(|turn| turn.directory)
            }
            Mode::RolledBack { .. } => None,
        };
        let Some(directory) = directory else {
            return path.to_string();
        };
        abs_path
            .strip_prefix(&directory)
            .ok()
            .map(|path| path.to_string_lossy().replace('\\', "/"))
            .filter(|path| !path.is_empty())
            .unwrap_or_else(|| abs_path.to_string_lossy().into_owned())
    }

    /// Puts the hunks back as the user last accepted them, on disk too.
    /// SOLO: Not in a file with unsaved edits or a conflict in its buffer
    /// (G59).
    pub(crate) fn reject_hunks(
        &mut self,
        ranges: Vec<Range<editor::Anchor>>,
        cx: &mut Context<Self>,
    ) {
        if !matches!(self.mode, Mode::Pending) {
            return;
        }
        let mut created = Vec::new();
        let mut unsaved = Vec::new();
        let mut saves = Vec::new();
        for (ix, hunks) in self.hunks_by_file(&ranges, cx) {
            let file = &self.files[ix];
            let buffer = file.buffer.read(cx);
            if buffer.is_dirty() || buffer.has_conflict() {
                unsaved.push(file.path_key.path.as_unix_str().to_string());
                continue;
            }
            if file.base.is_none() {
                // Accepted without the file: the agent created it.
                let since = file.pending.as_ref().and_then(|pending| pending.since);
                created.push((file.abs_path.clone(), file.path_key.clone(), since));
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
            let abs_path = file.abs_path.clone();
            let relative = file.path.clone();
            // SOLO: written byte for byte, not as the buffer saves it: that
            // would give every line the same line break. The buffer is
            // changed once the file is written, so nothing changes if it
            // can't be.
            if self.project.read(cx).is_local() && buffer.read(cx).encoding().name() == "UTF-8" {
                let path = self.agent_path(&abs_path, &relative, cx);
                let save = self.write_rejected(buffer, abs_path, &relative, path, hunks, cx);
                saves.push((save, relative));
                continue;
            }
            let text = buffer.update(cx, |buffer, cx| {
                buffer.edit(edits, None, cx);
                buffer.text()
            });
            // Where the rejected lines are now, for the agent.
            let snapshot = buffer.read(cx).snapshot();
            let path = self.agent_path(&abs_path, &relative, cx);
            let rejections: Vec<Rejection> = hunks
                .iter()
                .map(|hunk| {
                    let rows = point_rows(&hunk.buffer_range.to_point(&snapshot));
                    Rejection {
                        path: path.clone(),
                        lines: Some(rows.start + 1..rows.end + 1),
                    }
                })
                .collect();
            // Left as the review left it, here at once (so its own edit isn't
            // taken for someone else's); for the pending changes once it's
            // saved: until then the agent's change isn't undone (G59).
            let file = &mut self.files[ix];
            let update = file.pending.as_mut().map(|pending| {
                pending.left = Version::Text(text.as_str().into());
                let has_changes = pending.accepted != pending.left;
                (file.path.clone(), Some(pending.clone()), has_changes)
            });
            let save = self
                .project
                .update(cx, |project, cx| project.save_buffer(buffer, cx));
            let save = cx.background_spawn(async move {
                save.await?;
                anyhow::Ok((update, rejections))
            });
            saves.push((save, relative));
        }
        if !saves.is_empty() {
            self.save_rejections(saves, cx);
        }
        self.status = if unsaved.is_empty() {
            None
        } else if is_chinese() {
            Some(
                format!(
                    "{} 在编辑器里有没保存的改动或冲突，没有拒绝。请先保存或处理冲突。",
                    unsaved.join("、")
                )
                .into(),
            )
        } else {
            Some(
                format!(
                    "{} has unsaved edits or a conflict in the editor, so it wasn't rejected. Save it or resolve the conflict first.",
                    unsaved.join(", ")
                )
                .into(),
            )
        };
        if !created.is_empty() {
            self.trash_created_files(created, cx);
        }
        cx.notify();
    }

    /// SOLO: Rejects `hunks` of `buffer`'s file at `abs_path` (`path` in the
    /// work tree, `agent_path` for the agent): writes only the lines
    /// rejecting changes, as they are in the accepted version, so the rest
    /// keeps its line breaks and byte order mark (see
    /// [`pending::rejected_bytes`]); then makes the same edits in the
    /// buffer, and records them in the pending changes and for the agent.
    /// After an earlier rejection of the file, from what it left: the hunks
    /// are taken from the diff as it is then, so one rejected already isn't
    /// again. Fails, changing nothing, if the file isn't what the buffer
    /// has, or either changed meanwhile. Gives what the pending changes
    /// didn't take (without them, the page keeps it).
    fn write_rejected(
        &mut self,
        buffer: Entity<Buffer>,
        abs_path: PathBuf,
        path: &str,
        agent_path: String,
        hunks: Vec<Hunk>,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<RejectedFile>> {
        let fs = self.project.read(cx).fs().clone();
        let buffer_id = buffer.read(cx).remote_id();
        let terminal_id = self.terminal_id;
        let store_dir = self.store_dir.clone();
        let relative = path.to_string();
        // The accepted version as it was on disk is in the turns' snapshots,
        // when they have it: newest last.
        let turns: Vec<AgentTurn> = AgentTurns::try_global(cx)
            .map(|turns| turns.read(cx).turns(self.terminal_id).cloned().collect())
            .unwrap_or_default();
        let store = turns
            .iter()
            .find(|turn| Some(turn.store.store_dir()) == self.store_dir.as_deref())
            .map(|turn| turn.store.clone());
        let objects: Vec<String> = turns
            .iter()
            .filter(|turn| Some(turn.store.store_dir()) == self.store_dir.as_deref())
            .flat_map(|turn| {
                [
                    format!("{}:{path}", turn.before),
                    format!("{}:{path}", turn.after),
                ]
            })
            .collect();
        // One rejection of the file at a time.
        let previous = self.rejecting.get(&buffer_id).cloned();
        let (done, finished) = futures::channel::oneshot::channel::<()>();
        self.rejecting
            .insert(buffer_id, finished.map(|_| ()).boxed().shared());
        cx.spawn(async move |this, cx| {
            // Dropped when this one is over, however it ends.
            let _done = done;
            if let Some(previous) = previous {
                previous.await;
            }
            // The diff as the earlier rejection left it.
            let diff_update = this.update(cx, |this, cx| {
                let ix = this.file_index(buffer_id, cx)?;
                Some(this.files[ix].diff_update.clone())
            })?;
            let Some(diff_update) = diff_update else {
                anyhow::bail!(tr("it isn't shown anymore", "它已经不在页面上了"));
            };
            diff_update.await;
            // The hunks asked for that are still there, as they are now.
            let prepared = this.update(cx, |this, cx| {
                let ix = this.file_index(buffer_id, cx)?;
                let file = &this.files[ix];
                let buffer = buffer.read(cx);
                if buffer.is_dirty() || buffer.has_conflict() {
                    return Some(None);
                }
                let snapshot = buffer.snapshot();
                let diff = file.diff.read(cx);
                let base = diff.base_text(cx);
                let asked: Vec<(Range<usize>, Range<usize>)> = hunks
                    .iter()
                    .map(|hunk| {
                        (
                            hunk.buffer_range.to_offset(&snapshot),
                            hunk.base_range.clone(),
                        )
                    })
                    .collect();
                let mut edits: Vec<(Range<language::Anchor>, Range<usize>, String)> = diff
                    .snapshot(cx)
                    .hunks_intersecting_range(
                        language::Anchor::min_max_range_for_buffer(snapshot.remote_id()),
                        &snapshot,
                    )
                    .filter_map(|hunk| {
                        let offsets = hunk.buffer_range.to_offset(&snapshot);
                        let base_range = hunk.diff_base_byte_range.clone();
                        asked
                            .iter()
                            .any(|(asked_offsets, asked_base)| {
                                *asked_offsets == offsets || *asked_base == base_range
                            })
                            .then(|| {
                                let text: String = base.text_for_range(base_range).collect();
                                (hunk.buffer_range.clone(), offsets, text)
                            })
                    })
                    .collect();
                edits.sort_by_key(|(_, offsets, _)| offsets.start);
                Some(Some((
                    snapshot.version().clone(),
                    snapshot.text(),
                    edits,
                    base.text(),
                )))
            })?;
            let Some(prepared) = prepared else {
                anyhow::bail!(tr("it isn't shown anymore", "它已经不在页面上了"));
            };
            let Some((version, before, edits, accepted)) = prepared else {
                anyhow::bail!(tr(
                    "it has unsaved edits or a conflict in the editor",
                    "它在编辑器里有没保存的改动或冲突",
                ));
            };
            if edits.is_empty() {
                return Ok((None, Vec::new()));
            }
            let offset_edits: Vec<(Range<usize>, String)> = edits
                .iter()
                .map(|(_, offsets, text)| (offsets.clone(), text.clone()))
                .collect();
            let after_ranges = edits_after(&offset_edits);
            let disk = fs.load_bytes(&abs_path).await?;
            let (disk, after, bytes) = cx
                .background_spawn(async move {
                    let after = pending::apply_edits(&before, &offset_edits);
                    if std::str::from_utf8(&disk).is_err() {
                        anyhow::bail!(tr(
                            "it isn't UTF-8 text on disk",
                            "它在磁盘上不是 UTF-8 文本",
                        ));
                    }
                    let accepted_on_disk = match store {
                        Some(store) => accepted_on_disk(&store, objects),
                        None => Vec::new(),
                    };
                    let bytes = pending::rejected_bytes(
                        &disk,
                        &before,
                        &after,
                        &accepted,
                        &accepted_on_disk,
                    )
                    .context(changed_on_disk())?;
                    anyhow::Ok((disk, after, bytes))
                })
                .await?;
            // Still what it was worked out from, on disk and in the editor.
            if fs.load_bytes(&abs_path).await? != disk {
                anyhow::bail!(changed_on_disk());
            }
            let unchanged = buffer.read_with(cx, |buffer, _| {
                buffer.version() == version && !buffer.is_dirty() && !buffer.has_conflict()
            });
            if !unchanged {
                anyhow::bail!(tr(
                    "it changed in the editor meanwhile; try again",
                    "这期间它在编辑器里改过了，请再试一次",
                ));
            }
            fs.write(&abs_path, &bytes).await?;
            #[cfg(test)]
            this.update(cx, |this, cx| {
                if let Some(after_write) = this.after_write.take() {
                    after_write(this, cx);
                }
            })
            .ok();
            let rejections: Vec<Rejection> = offset_edits_rows(&after, &after_ranges)
                .into_iter()
                .map(|rows| Rejection {
                    path: agent_path.clone(),
                    lines: Some(rows.start + 1..rows.end + 1),
                })
                .collect();
            let left = Version::Text(after.as_str().into());
            // Written: the buffer has the same edits at once, before its file
            // watcher reloads it (which would take them for a conflict).
            let recorded = cx.update(|cx| {
                if buffer.read(cx).version() == version {
                    let edits: Vec<(Range<language::Anchor>, String)> = edits
                        .into_iter()
                        .map(|(range, _, text)| (range, text))
                        .collect();
                    buffer.update(cx, |buffer, cx| {
                        buffer.edit(edits, None, cx);
                        let version = buffer.version();
                        // The time it was saved before: not newer than what
                        // was written, so no conflict until the reload.
                        let mtime = buffer.saved_mtime();
                        buffer.did_save(version, mtime, cx);
                    });
                }
                // Left as the review left it, here at once (so its own edit
                // isn't taken for someone else's): only that, on the pending
                // file as it is now (a hunk kept meanwhile stays kept).
                let on_page = this
                    .update(cx, |this, cx| {
                        let ix = this.file_index(buffer_id, cx)?;
                        let pending = this.files[ix].pending.as_mut()?;
                        pending.left = left.clone();
                        Some(pending.clone())
                    })
                    .ok()
                    .flatten();
                // Recorded, also when the page is gone.
                let (Some(store_dir), Some(turns)) =
                    (store_dir, AgentTurns::try_global(cx).cloned())
                else {
                    let update = on_page.map(|pending| {
                        let has_changes = pending.accepted != pending.left;
                        (relative.clone(), Some(pending), has_changes)
                    });
                    return Some(update);
                };
                let pending = on_page.or_else(|| {
                    let mut pending = turns
                        .read(cx)
                        .pending_changes_in(terminal_id, &store_dir)?
                        .files
                        .get(&relative)?
                        .clone();
                    pending.left = left.clone();
                    Some(pending)
                });
                let update = pending.map(|pending| {
                    let has_changes = pending.accepted != pending.left;
                    (relative.clone(), Some(pending), has_changes)
                });
                let added = turns.update(cx, |turns, cx| {
                    if let Some(update) = update.clone() {
                        turns.update_pending_files(terminal_id, &store_dir, vec![update], cx);
                    }
                    turns.add_rejections(terminal_id, &store_dir, rejections.clone(), cx)
                });
                if added { None } else { Some(update) }
            });
            match recorded {
                None => Ok((None, Vec::new())),
                // Kept by the page instead.
                Some(update) => Ok((update, rejections)),
            }
        })
    }

    /// SOLO: Once the rejected files are saved, the pending changes and the
    /// message for the agent have them; a file that failed to save keeps
    /// the agent's change pending, and the page says why (G59).
    fn save_rejections(
        &mut self,
        saves: Vec<(Task<anyhow::Result<RejectedFile>>, String)>,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            let mut updates = Vec::new();
            let mut rejected = Vec::new();
            let mut failed = Vec::new();
            for (save, path) in saves {
                match save.await {
                    Ok((update, rejections)) => {
                        updates.extend(update);
                        rejected.extend(rejections);
                    }
                    Err(error) => {
                        log::warn!("failed to save the rejected {path}: {error:#}");
                        failed.push(format!("{path} ({error:#})"));
                    }
                }
            }
            this.update(cx, |this, cx| {
                this.add_rejected(rejected, cx);
                if !updates.is_empty() {
                    this.model_update(updates, cx);
                }
                if !failed.is_empty() {
                    let message = if is_chinese() {
                        format!("没有拒绝：{}", failed.join("、"))
                    } else {
                        format!("Not rejected: {}", failed.join(", "))
                    };
                    this.status = Some(match this.status.take() {
                        Some(status) => format!("{status} {message}").into(),
                        None => message.into(),
                    });
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// SOLO: Moves files the agent created to the recycle bin, rather than
    /// deleting them for good, and stops showing them. Files that were there
    /// before the turn although its first snapshot doesn't have them (ignored
    /// then, say, by a rule the turn took out of .gitignore) are left alone.
    fn trash_created_files(
        &mut self,
        files: Vec<(PathBuf, PathKey, Option<SystemTime>)>,
        cx: &mut Context<Self>,
    ) {
        let fs = self.project.read(cx).fs().clone();
        let work_tree = self.work_tree.clone();
        let previous = std::mem::replace(&mut self.trash_task, Task::ready(()));
        self.trash_task = cx.spawn(async move |this, cx| {
            previous.await;
            let mut removed = Vec::new();
            let mut kept = Vec::new();
            for (abs_path, path_key, started_at) in files {
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
                let mut updates = Vec::new();
                for path_key in removed {
                    if let Some(ix) = this.files.iter().position(|file| file.path_key == path_key) {
                        let file = this.files.remove(ix);
                        let path = this.agent_path(&file.abs_path, &file.path, cx);
                        this.add_rejected(vec![Rejection { path, lines: None }], cx);
                        // Accepted without it, and it's gone: nothing's left.
                        updates.push((file.path, None, false));
                    }
                    this.multibuffer.update(cx, |multibuffer, cx| {
                        multibuffer.remove_excerpts_for_paths(vec![path_key], cx)
                    });
                }
                if !updates.is_empty() {
                    this.model_update(updates, cx);
                }
                if !kept.is_empty() {
                    let message = if is_chinese() {
                        format!("{} 可能在这一轮之前就已存在，没有删除。", kept.join("、"))
                    } else {
                        format!(
                            "{} may have existed before the turn, so it wasn't deleted.",
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

    /// Takes the hunks as they are now: they stop showing as changes, and
    /// SOLO: the user accepted them (G59).
    pub(crate) fn keep_hunks(
        &mut self,
        ranges: Vec<Range<editor::Anchor>>,
        cx: &mut Context<Self>,
    ) {
        if !matches!(self.mode, Mode::Pending) {
            return;
        }
        let mut updates = Vec::new();
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
            let current = buffer_version(&file.buffer, cx);
            let file = &mut self.files[ix];
            // A deleted file whose deletion is kept is accepted gone.
            let accepted = if base.is_empty() && current == Some(Version::Absent) {
                file.base = None;
                Version::Absent
            } else {
                file.base = Some(base.as_str().into());
                Version::Text(base.into())
            };
            if let Some(pending) = file.pending.as_mut() {
                pending.accepted = accepted;
                if let Some(current) = current {
                    pending.left = current;
                }
                let has_changes = pending.accepted != pending.left;
                updates.push((file.path.clone(), Some(pending.clone()), has_changes));
            }
            self.update_diff(ix, cx);
        }
        if !updates.is_empty() {
            self.model_update(updates, cx);
        }
        cx.notify();
    }

    /// SOLO: Keeps everything shown, and the files that aren't text.
    fn keep_all(&mut self, cx: &mut Context<Self>) {
        self.keep_hunks(Self::all_hunks(), cx);
        let Some(changes) = AgentTurns::try_global(cx)
            .and_then(|turns| turns.read(cx).pending_changes(self.terminal_id).cloned())
        else {
            return;
        };
        // Not other turns' files, when filtered by turn.
        if self.filter != TurnFilter::All {
            return;
        }
        let binary: Vec<String> = changes
            .files
            .iter()
            .filter(|(path, file)| {
                !self.locked.contains(path)
                    && (matches!(file.accepted, Version::Binary(_))
                        || matches!(file.left, Version::Binary(_)))
            })
            .map(|(path, _)| path.clone())
            .collect();
        if !binary.is_empty()
            && let Some(store_dir) = self.store_dir.clone()
        {
            let terminal_id = self.terminal_id;
            AgentTurns::global(cx).update(cx, |turns, cx| {
                turns.accept_as_they_are(terminal_id, &store_dir, binary, cx)
            });
        }
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
        // SOLO: what was pending, to put back if it's undone (G59).
        let previous = AgentTurns::try_global(cx).and_then(|turns| {
            turns
                .read(cx)
                .pending_changes_in(self.terminal_id, store.store_dir())
                .map(|changes| changes.files.clone())
        });
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
                    let pin = Arc::new(store.pin(&restored.undo));
                    // SOLO: what's rolled back is accepted as it is now.
                    let paths: Vec<String> =
                        restored.changed.iter().map(|file| file.path.clone()).collect();
                    let terminal_id = this.terminal_id;
                    let store_dir = store.store_dir().to_path_buf();
                    AgentTurns::global(cx).update(cx, |turns, cx| {
                        turns.accept_as_they_are(terminal_id, &store_dir, paths, cx)
                    });
                    this.set_mode(
                        Mode::RolledBack {
                            number,
                            store,
                            undo: restored.undo,
                            _undo_pin: pin,
                            file_count,
                            stopped_partway: false,
                            previous,
                        },
                        window,
                        cx,
                    );
                    this.status = join_messages([
                        skipped_message(&restored.skipped),
                        large_files_message(&restored.large_files),
                    ]);
                }
                Err(error) if error.downcast_ref::<PartlyRestored>().is_some() => {
                    let Ok(partly) = error.downcast::<PartlyRestored>() else {
                        return;
                    };
                    // SOLO: keep the way back: the files are half rolled back.
                    let pin = Arc::new(store.pin(&partly.undo));
                    this.set_mode(
                        Mode::RolledBack {
                            number,
                            store,
                            undo: partly.undo,
                            _undo_pin: pin,
                            file_count: 0,
                            stopped_partway: true,
                            previous: None,
                        },
                        window,
                        cx,
                    );
                    this.status = Some(
                        format!(
                            "{}: {:#}. {}",
                            tr("Rollback stopped partway", "回滚中途失败"),
                            partly.error,
                            tr(
                                "Some files were rolled back and some weren't. Click Undo Rollback to restore them all.",
                                "一部分文件已回滚，一部分没有。点“撤销回滚”可以全部恢复。",
                            )
                        )
                        .into(),
                    );
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
        let Mode::RolledBack {
            store,
            undo,
            previous,
            ..
        } = self.mode.clone()
        else {
            return;
        };
        self.status = Some(tr("Undoing the rollback…", "正在撤销回滚…").into());
        let store_dir = store.store_dir().to_path_buf();
        self.action_task = cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move { store.restore(&undo) })
                .await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(restored) => {
                    // SOLO: what was pending is again (G59).
                    if let Some(previous) = previous {
                        let terminal_id = this.terminal_id;
                        AgentTurns::global(cx).update(cx, |turns, cx| {
                            turns.replace_pending_files(terminal_id, &store_dir, previous, cx)
                        });
                    }
                    this.back_to_pending(window, cx);
                    let undone = tr("The rollback was undone.", "已撤销回滚。");
                    this.status = join_messages([
                        Some(undone.into()),
                        skipped_message(&restored.skipped),
                        large_files_message(&restored.large_files),
                    ]);
                }
                Err(error) => {
                    // SOLO: the way back is kept, to try again (R3 M2).
                    let message = if error.downcast_ref::<PartlyRestored>().is_some() {
                        format!(
                            "{}: {error:#}. {}",
                            tr("Undo stopped partway", "撤销中途失败"),
                            tr(
                                "Some files are back and some aren't. Close any program that has them open, then click Undo Rollback again.",
                                "一部分文件已恢复，一部分没有。请关掉打开着这些文件的程序，再点一次“撤销回滚”。",
                            )
                        )
                    } else {
                        format!("{}: {error:#}", tr("Undo failed", "撤销失败"))
                    };
                    this.status = Some(message.into());
                    cx.notify();
                }
            })
            .ok();
        });
        cx.notify();
    }

    /// SOLO: Review comments of the editor, with where they are.
    fn review_comments(
        &self,
        comments: Vec<(Range<editor::Anchor>, String)>,
        cx: &App,
    ) -> Vec<ReviewComment> {
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
                let path = self.agent_path(&file.abs_path, &file.path, cx);
                Some(ReviewComment {
                    path,
                    lines: start.row + 1..=end.row + 1,
                    code,
                    comment,
                })
            })
            .collect()
    }

    /// Sends the review comments, SOLO: and the hunks rejected, to the
    /// agent's terminal as one message (G59).
    fn send_to_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // SOLO: those from before first, as they were written first.
        let mut comments = self.carried.clone();
        let in_editor = self.editor.read(cx).review_comments();
        comments.extend(self.review_comments(in_editor, cx));
        let rejected = self.unsent_rejected(cx);
        if comments.is_empty() && rejected.is_empty() {
            return;
        }
        let message = agent_message(&comments, &rejected);
        let sent = tr("Sent to the agent.", "已发送给 agent。").to_string();
        if !self.paste_to_agent(message, sent, window, cx) {
            // SOLO: only copied: still to send (G59).
            let kept = tr(
                "They're still here, to send later.",
                "这些内容仍保留，之后可以再发送。",
            );
            self.status = self
                .status
                .take()
                .map(|status| format!("{status} {kept}").into());
            return;
        }
        // Sent: gone from here and from what's kept.
        self.carried.clear();
        self.carried_rows.clear();
        self.rejected.clear();
        self.editor
            .update(cx, |editor, cx| editor.take_review_comments(cx));
        let terminal_id = self.terminal_id;
        if let Some(turns) = AgentTurns::try_global(cx).cloned() {
            turns.update(cx, |turns, cx| turns.take_unsent(terminal_id, cx));
        }
    }

    /// SOLO: Notes hunks rejected, for the next message to the agent: with
    /// the pending changes, so they're still there after the page is closed
    /// or Solo restarts (G59).
    fn add_rejected(&mut self, rejected: Vec<Rejection>, cx: &mut Context<Self>) {
        if rejected.is_empty() {
            return;
        }
        let terminal_id = self.terminal_id;
        let kept = match (self.store_dir.clone(), AgentTurns::try_global(cx).cloned()) {
            (Some(store_dir), Some(turns)) => turns.update(cx, |turns, cx| {
                turns.add_rejections(terminal_id, &store_dir, rejected.clone(), cx)
            }),
            _ => false,
        };
        if !kept {
            for rejection in rejected {
                if !self.rejected.contains(&rejection) {
                    self.rejected.push(rejection);
                }
            }
        }
        cx.notify();
    }

    /// SOLO: The hunks rejected since the last message to the agent.
    fn unsent_rejected(&self, cx: &App) -> Vec<Rejection> {
        let mut rejected = AgentTurns::try_global(cx)
            .map(|turns| turns.read(cx).unsent(self.terminal_id).rejected)
            .unwrap_or_default();
        rejected.extend(self.rejected.iter().cloned());
        rejected
    }

    /// SOLO: Keeps the comments not sent with the pending changes, as they
    /// are now (G59).
    fn keep_comments(&mut self, cx: &mut Context<Self>) {
        if !self.pending_page || !matches!(self.mode, Mode::Pending) {
            return;
        }
        let Some(store_dir) = self.store_dir.clone() else {
            return;
        };
        let mut comments = self.carried.clone();
        let in_editor = self.editor.read(cx).review_comments();
        comments.extend(self.review_comments(in_editor, cx));
        let terminal_id = self.terminal_id;
        AgentTurns::global(cx).update(cx, |turns, cx| {
            turns.set_unsent_comments(terminal_id, &store_dir, comments, cx)
        });
    }

    /// SOLO: Looks for where the comments from before are now, in the
    /// background: the lines they were on, if those still have the code
    /// they were on, else the nearest lines that do (see [`comment_rows`]).
    fn find_carried_comments(&mut self, cx: &mut Context<Self>) {
        if self.carried.is_empty() {
            return;
        }
        let files: Vec<(String, language::BufferId, String)> = self
            .files
            .iter()
            .map(|file| {
                let buffer = file.buffer.read(cx);
                (
                    self.agent_path(&file.abs_path, &file.path, cx),
                    buffer.remote_id(),
                    buffer.text(),
                )
            })
            .collect();
        let carried = self.carried.clone();
        self.carried_search = cx.spawn(async move |this, cx| {
            let rows = cx
                .background_spawn({
                    let carried = carried.clone();
                    async move {
                        carried
                            .iter()
                            .map(|comment| {
                                let (_, buffer_id, text) =
                                    files.iter().find(|(path, _, _)| *path == comment.path)?;
                                Some((*buffer_id, comment_rows(text, comment)?))
                            })
                            .collect::<Vec<_>>()
                    }
                })
                .await;
            this.update(cx, |this, cx| {
                // Not if they changed meanwhile: looked for again then.
                if this.carried != carried {
                    return;
                }
                this.carried_rows = rows;
                this.place_carried_comments(cx);
            })
            .ok();
        });
    }

    /// SOLO: Puts the comments from before in the editor where they were
    /// found, once those lines show, and shows them there.
    fn place_carried_comments(&mut self, cx: &mut Context<Self>) {
        if self.carried.is_empty()
            || self.placing
            || self.carried_rows.len() != self.carried.len()
            || !matches!(self.mode, Mode::Pending)
        {
            return;
        }
        let snapshot = self.multibuffer.read(cx).snapshot(cx);
        let mut placed = Vec::new();
        for (ix, found) in self.carried_rows.iter().enumerate() {
            let Some((buffer_id, rows)) = found else {
                continue;
            };
            let Some(file_ix) = self.file_index(*buffer_id, cx) else {
                continue;
            };
            let buffer = self.files[file_ix].buffer.read(cx).snapshot();
            if rows.end > buffer.max_point().row {
                continue;
            }
            let start = buffer.anchor_after(Point::new(rows.start, 0));
            let end = buffer.anchor_before(Point::new(rows.end, buffer.line_len(rows.end)));
            let (Some(start), Some(end)) = (
                snapshot.anchor_in_excerpt(start),
                snapshot.anchor_in_excerpt(end),
            ) else {
                continue;
            };
            placed.push((self.carried[ix].clone(), start..end));
        }
        if placed.is_empty() {
            return;
        }
        // Shown in the editor's review overlays, which need the window: not
        // while this is being updated. They stay among those from before
        // until they're there.
        self.placing = true;
        let editor = self.editor.clone();
        let window = self.window;
        let this = cx.weak_entity();
        cx.defer(move |cx| {
            let shown = window
                .update(cx, |_, window, cx| {
                    editor.update(cx, |editor, cx| {
                        for (comment, range) in &placed {
                            editor.restore_review_comment(
                                range.clone(),
                                comment.comment.clone(),
                                window,
                                cx,
                            );
                        }
                    })
                })
                .log_err()
                .is_some();
            this.update(cx, |this, cx| {
                this.placing = false;
                if shown {
                    for (comment, _) in &placed {
                        if let Some(ix) = this.carried.iter().position(|carried| carried == comment)
                        {
                            this.carried.remove(ix);
                            if ix < this.carried_rows.len() {
                                this.carried_rows.remove(ix);
                            }
                        }
                    }
                }
                // What's kept: the editor's, and those from before not in it.
                this.keep_comments(cx);
                cx.notify();
            })
            .ok();
        });
    }

    /// Sends the problems the last turn added to its agent, to fix them.
    fn send_diagnostics(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(turn) = self.diagnostics_turn(cx) else {
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

    /// SOLO: The turn whose diagnostics are shown: the one shown, or with
    /// the pending changes, the last one that finished.
    fn diagnostics_turn(&self, cx: &App) -> Option<AgentTurn> {
        match self.mode {
            Mode::Turn(number) => self.turn(number, cx),
            Mode::Pending => {
                let turns = AgentTurns::try_global(cx)?.read(cx);
                let finished = turns
                    .terminals
                    .get(&self.terminal_id)
                    .map(super::TerminalTurns::finished_turns)?;
                turns
                    .turns(self.terminal_id)
                    .last()
                    .filter(|turn| turn.number == finished)
                    .cloned()
            }
            _ => None,
        }
    }

    /// Pastes `message` into the agent's prompt, or else copies it. SOLO:
    /// Whether it pasted it.
    fn paste_to_agent(
        &mut self,
        message: String,
        sent: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
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
            return false;
        };
        if !crate::agent_context::can_paste_to_agent(terminal_view.read(cx), &message, cx) {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(message));
            self.status = Some(
                tr(
                    "The agent isn't running; copied the message instead.",
                    "agent 没有在运行，已改为复制。",
                )
                .into(),
            );
            cx.notify();
            return false;
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
        true
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
        let turn = self.diagnostics_turn(cx)?;
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
                return muted(tr("Diagnostics: not checked", "诊断：未检查").to_string());
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
                "第 {} 轮新增 {} 个错误、{warnings} 个警告，修掉了 {fixed_errors} 个错误",
                turn.number,
                errors + added_uncertain
            )
        } else {
            format!(
                "Turn {} added {} errors and {warnings} warnings, and fixed {fixed_errors} errors",
                turn.number,
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
                                    "Send the new problems to the agent to fix",
                                    "把新增的问题发给 agent 修复",
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
                                    "另有 {added_uncertain} 个新错误在改动前没打开的文件里"
                                )
                            } else {
                                format!(
                                    "and {added_uncertain} more errors in files that weren't open before the turn"
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

    /// SOLO: Whose changes the turn shown has, when it may not be only the
    /// agent's (R3 M5).
    fn attribution_note(&self, cx: &App) -> Option<String> {
        let Mode::Turn(number) = self.mode else {
            return None;
        };
        let turn = self.turn(number, cx)?;
        let mut notes = Vec::new();
        if !turn.attributed {
            notes.push(
                tr(
                    "These are all the files that changed during the turn; some may not be this agent's.",
                    "这里是这一轮期间改动过的所有文件，其中有些可能不是这个 agent 改的。",
                )
                .to_string(),
            );
        } else if !turn.others.is_empty() {
            const LISTED: usize = 5;
            let mut names: Vec<&str> = turn
                .others
                .iter()
                .take(LISTED)
                .map(|file| file.path.as_str())
                .collect();
            if turn.others.len() > LISTED {
                names.push("…");
            }
            let names = names.join(", ");
            let count = turn.others.len();
            notes.push(if is_chinese() {
                format!(
                    "另有 {count} 个文件在这一轮期间改动过，但 agent 没有报告编辑过它们，这里不显示：{names}"
                )
            } else {
                format!(
                    "{count} more files changed during the turn that the agent didn't report editing; they aren't shown: {names}"
                )
            });
        }
        if !turn.edited_before_snapshot.is_empty() {
            notes.push(
                tr(
                    "Some changes made at the start of the turn may show only in part.",
                    "这一轮开头的一些改动可能只显示了一部分。",
                )
                .to_string(),
            );
        }
        (!notes.is_empty()).then(|| notes.join(" "))
    }

    /// SOLO: "Send to Agent (1 comment, 1 rejection)" (G59).
    fn send_label(&self, cx: &App) -> String {
        let comments = self.comment_count + self.carried.len();
        let rejected = self.unsent_rejected(cx).len();
        let mut parts = Vec::new();
        if is_chinese() {
            if comments > 0 {
                parts.push(format!("{comments} 条批注"));
            }
            if rejected > 0 {
                parts.push(format!("拒绝 {rejected} 处"));
            }
            format!("发送给 agent（{}）", parts.join("、"))
        } else {
            if comments > 0 {
                parts.push(if comments == 1 {
                    "1 comment".to_string()
                } else {
                    format!("{comments} comments")
                });
            }
            if rejected > 0 {
                parts.push(if rejected == 1 {
                    "1 rejection".to_string()
                } else {
                    format!("{rejected} rejections")
                });
            }
            format!("Send to Agent ({})", parts.join(", "))
        }
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let history_count = AgentTurns::try_global(cx)
            .map(|turns| turns.read(cx).turns(self.terminal_id).count())
            .unwrap_or_default();
        let history_button = Button::new(
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
        }));
        let (title, detail, read_only, actions): (String, Option<String>, bool, AnyElement) =
            match &self.mode {
                Mode::Pending => {
                    let count = AgentTurns::try_global(cx)
                        .map(|turns| turns.read(cx).pending_file_count(self.terminal_id))
                        .unwrap_or_default();
                    let detail = if is_chinese() {
                        format!("{count} 个文件")
                    } else if count == 1 {
                        "1 file".to_string()
                    } else {
                        format!("{count} files")
                    };
                    let has_hunks = !self.multibuffer.read(cx).is_empty();
                    let send = self.comment_count > 0
                        || !self.carried.is_empty()
                        || !self.unsent_rejected(cx).is_empty();
                    let actions = h_flex()
                        .gap_1()
                        .when(send, |this| {
                            this.child(
                                Button::new("send-to-agent", self.send_label(cx))
                                    .style(ButtonStyle::Filled)
                                    .tooltip(Tooltip::text(tr(
                                        "Paste the comments and what you rejected into the agent's prompt, without sending it",
                                        "把批注和拒绝的改动合成一条消息，粘贴进 agent 的输入框（不会自动回车）",
                                    )))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.send_to_agent(window, cx)
                                    })),
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
                                .disabled(!has_hunks && self.skipped_files == 0)
                                .on_click(cx.listener(|this, _, _, cx| this.keep_all(cx))),
                        )
                        .child(history_button)
                        .into_any_element();
                    (
                        tr("Pending Changes", "待审改动").to_string(),
                        Some(detail),
                        false,
                        actions,
                    )
                }
                Mode::Turn(number) => {
                    let number = *number;
                    let title = self
                        .turn(number, cx)
                        .map(|turn| turn_title(&turn))
                        .unwrap_or_default();
                    let actions = h_flex()
                        .gap_1()
                        .child(
                            Button::new(
                                "roll-back-shown-turn",
                                tr("Roll Back to Before", "回滚到这一轮之前"),
                            )
                            .tooltip(Tooltip::text(tr(
                                "Shows what would be discarded first",
                                "会先展示将被丢弃的改动",
                            )))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.set_mode(Mode::ConfirmRollback(number), window, cx)
                            })),
                        )
                        .child(
                            Button::new("back-to-pending", tr("Back to Pending", "返回待审"))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.back_to_pending(window, cx)
                                })),
                        )
                        .into_any_element();
                    (title, None, true, actions)
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
                    let actions = h_flex()
                        .gap_1()
                        .child(
                            Button::new("cancel-rollback", tr("Cancel", "取消")).on_click(
                                cx.listener(move |this, _, window, cx| {
                                    this.back_to_pending(window, cx)
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
                    (title, None, false, actions)
                }
                Mode::RolledBack {
                    number,
                    file_count,
                    stopped_partway,
                    ..
                } => {
                    let title = if *stopped_partway {
                        if is_chinese() {
                            format!("回滚到第 {number} 轮开始之前时中途失败")
                        } else {
                            format!("Rolling back to before turn {number} stopped partway")
                        }
                    } else if is_chinese() {
                        format!("已回滚到第 {number} 轮开始之前（{file_count} 个文件）")
                    } else {
                        format!("Rolled back to before turn {number} ({file_count} files)")
                    };
                    let actions = h_flex()
                        .gap_1()
                        .child(
                            Button::new("undo-rollback", tr("Undo Rollback", "撤销回滚"))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.undo_rollback(window, cx)
                                })),
                        )
                        .child(
                            Button::new("back-to-pending", tr("Back to Pending", "返回待审"))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.back_to_pending(window, cx)
                                })),
                        )
                        .into_any_element();
                    (title, None, false, actions)
                }
            };
        let sources = self.turn_sources(cx);

        v_flex()
            .w_full()
            .px_3()
            .py_2()
            .gap_2()
            .border_b_1()
            .border_color(cx.theme().colors().border)
            .bg(cx.theme().colors().editor_background)
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .justify_between()
                    .child(
                        h_flex()
                            .min_w_0()
                            .gap_2()
                            .child(Label::new(title).single_line().truncate())
                            .when_some(detail, |this, detail| {
                                this.child(
                                    Label::new(detail)
                                        .size(LabelSize::Small)
                                        .color(Color::Muted),
                                )
                            })
                            .when(read_only, |this| {
                                this.child(
                                    h_flex()
                                        .flex_none()
                                        .px_1p5()
                                        .rounded_full()
                                        .border_1()
                                        .border_color(cx.theme().colors().border)
                                        .bg(cx.theme().colors().element_background)
                                        .child(
                                            Label::new(tr("Read Only", "只读"))
                                                .size(LabelSize::XSmall)
                                                .color(Color::Muted),
                                        ),
                                )
                            }),
                    )
                    .child(h_flex().flex_none().gap_1().child(actions)),
            )
            .when(self.shows_turns(cx), |this| {
                this.child(self.render_filters(&sources, cx))
            })
            .when_some(self.status.clone(), |this, status| {
                this.child(
                    Label::new(status)
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
            })
            .children(
                matches!(self.mode, Mode::Pending)
                    .then(|| {
                        AgentTurns::try_global(cx)?
                            .read(cx)
                            .pending_changes(self.terminal_id)?
                            .save_error
                            .clone()
                    })
                    .flatten()
                    .map(|error| {
                        // SOLO: they'd be gone after a restart (G59).
                        Label::new(format!(
                            "{} {error}",
                            tr(
                                "The pending changes couldn't be recorded for after a restart; trying again:",
                                "待审改动没能记下来（重启后会丢），稍后再试：",
                            )
                        ))
                        .size(LabelSize::Small)
                        .color(Color::Warning)
                    }),
            )
            .children(self.attribution_note(cx).map(|note| {
                Label::new(note)
                    .size(LabelSize::Small)
                    .color(Color::Warning)
            }))
            .when(
                !matches!(self.mode, Mode::RolledBack { .. }) && !self.large_files.is_empty(),
                |this| {
                    this.children(large_files_message(&self.large_files).map(|message| {
                        Label::new(message)
                            .size(LabelSize::Small)
                            .color(Color::Warning)
                    }))
                },
            )
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

    /// SOLO: A chip for each turn the pending changes are from, to show
    /// only its changes (G59).
    fn render_filters(&self, sources: &[TurnFilter], cx: &mut Context<Self>) -> AnyElement {
        let chip = |id: SharedString,
                    label: String,
                    filter: TurnFilter,
                    running: bool,
                    cx: &mut Context<Self>| {
            let selected = self.filter == filter;
            let colors = cx.theme().colors();
            h_flex()
                .id(id)
                .h(rems_from_px(22_f32))
                .px_2()
                .gap_1()
                .rounded_full()
                .border_1()
                .cursor_pointer()
                .when(selected, |this| {
                    this.bg(colors.element_selected)
                        .border_color(colors.element_selected)
                })
                .when(!selected, |this| {
                    this.border_color(colors.border)
                        .hover(|style| style.bg(colors.element_hover))
                })
                .when(running, |this| {
                    this.child(
                        Icon::new(IconName::LoadCircle)
                            .size(IconSize::XSmall)
                            .color(Color::Accent)
                            .with_rotate_animation(2),
                    )
                })
                .child(
                    Label::new(label)
                        .size(LabelSize::Small)
                        .color(if selected { Color::Default } else { Color::Muted }),
                )
                .on_click(cx.listener(move |this, _, _, cx| this.set_filter(filter, cx)))
                .into_any_element()
        };
        let mut chips = vec![chip(
            "turn-filter-all".into(),
            tr("All", "全部").to_string(),
            TurnFilter::All,
            false,
            cx,
        )];
        let running_number = AgentTurns::try_global(cx).and_then(|turns| {
            turns
                .read(cx)
                .active_turns(self.terminal_id)
                .first()
                .map(|turn| turn.number)
        });
        for source in sources {
            match source {
                TurnFilter::Turn(pending::WHILE_CLOSED) => {
                    chips.push(chip(
                        "turn-filter-while-closed".into(),
                        while_closed_label().to_string(),
                        *source,
                        false,
                        cx,
                    ));
                }
                TurnFilter::Turn(number) => {
                    let time = self
                        .turn(*number, cx)
                        .map(|turn| time_format::format_time(turn.started_at));
                    let label = match (is_chinese(), time) {
                        (true, Some(time)) => format!("第 {number} 轮 · {time}"),
                        (true, None) => format!("第 {number} 轮"),
                        (false, Some(time)) => format!("Turn {number} · {time}"),
                        (false, None) => format!("Turn {number}"),
                    };
                    chips.push(chip(
                        format!("turn-filter-{number}").into(),
                        label,
                        *source,
                        false,
                        cx,
                    ));
                }
                TurnFilter::Running => {
                    let label = match (is_chinese(), running_number) {
                        (true, Some(number)) => format!("第 {number} 轮 · 进行中"),
                        (true, None) => "进行中".to_string(),
                        (false, Some(number)) => format!("Turn {number} · Running"),
                        (false, None) => "Running".to_string(),
                    };
                    chips.push(chip("turn-filter-running".into(), label, *source, true, cx));
                }
                TurnFilter::All => {}
            }
        }
        h_flex().gap_1p5().children(chips).into_any_element()
    }

    /// SOLO: The pending files the agent is changing now: their changes show
    /// once its turn ends (G59).
    fn render_locked_files(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        if !matches!(self.mode, Mode::Pending)
            || matches!(self.filter, TurnFilter::Turn(_))
        {
            return Vec::new();
        }
        let colors = cx.theme().colors();
        self.locked
            .iter()
            .map(|path| {
                let (directory, name) = match path.rsplit_once('/') {
                    Some((directory, name)) => (directory.to_string(), name.to_string()),
                    None => (String::new(), path.clone()),
                };
                h_flex()
                    .w_full()
                    .h(rems_from_px(32_f32))
                    .px_3()
                    .gap_2()
                    .border_b_1()
                    .border_color(colors.border_variant)
                    .bg(colors.editor_subheader_background)
                    .child(
                        Icon::new(IconName::File)
                            .size(IconSize::Small)
                            .color(Color::Muted),
                    )
                    .child(Label::new(name).size(LabelSize::Small))
                    .child(
                        Label::new(directory)
                            .size(LabelSize::XSmall)
                            .color(Color::Muted)
                            .truncate(),
                    )
                    .child(div().flex_1())
                    .child(
                        h_flex()
                            .flex_none()
                            .gap_1()
                            .child(
                                Icon::new(IconName::LoadCircle)
                                    .size(IconSize::XSmall)
                                    .color(Color::Accent)
                                    .with_rotate_animation(2),
                            )
                            .child(
                                Label::new(tr("Agent is changing it", "agent 正在改"))
                                    .size(LabelSize::XSmall)
                                    .color(Color::Muted),
                            ),
                    )
                    .into_any_element()
            })
            .collect()
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
                                    .tooltip(Tooltip::text(tr(
                                        "What this turn changed, read only",
                                        "只读：这一轮自己改了什么",
                                    )))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.open_turn(Mode::Turn(number), window, cx)
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
                                        this.open_turn(Mode::ConfirmRollback(number), window, cx)
                                    },
                                )),
                            ),
                    )
                    .into_any_element()
            })
            .collect()
    }
}

/// SOLO: The rows (from 0: the first and the last) of `text` a comment from
/// before is on: the lines it was on, if they still have the code it was
/// on; else the nearest lines in the file that do.
fn comment_rows(text: &str, comment: &ReviewComment) -> Option<Range<u32>> {
    let lines: Vec<&str> = text.split('\n').collect();
    let code: Vec<&str> = comment.code.split('\n').collect();
    let count = code.len();
    let start = comment.lines.start().saturating_sub(1) as usize;
    let matches = |row: usize| {
        lines
            .get(row..row + count)
            .is_some_and(|found| found == code)
    };
    (0..lines.len())
        .filter(|row| matches(*row))
        .min_by_key(|row| row.abs_diff(start))
        .map(|row| row as u32..(row + count - 1) as u32)
}

/// SOLO: A review page of the terminal closed (G59).
fn close_review(terminal_id: EntityId, cx: &mut App) {
    if let Some(turns) = AgentTurns::try_global(cx).cloned() {
        cx.defer(move |cx| turns.update(cx, |turns, cx| turns.review_closed(terminal_id, cx)));
    }
}

/// SOLO: The rows of a range of points: those with any of it.
fn point_rows(range: &Range<Point>) -> Range<u32> {
    let end = if range.end.column > 0 {
        range.end.row + 1
    } else {
        range.end.row
    };
    range.start.row..end.max(range.start.row)
}

/// SOLO: "Turn 1" or "Turns 1, 2"; `None` when not known (G59).
fn turns_label(turns: &BTreeSet<usize>) -> Option<String> {
    if turns.is_empty() {
        return None;
    }
    let while_closed = turns.contains(&pending::WHILE_CLOSED);
    let numbers: Vec<String> = turns
        .iter()
        .filter(|number| **number != pending::WHILE_CLOSED)
        .map(usize::to_string)
        .collect();
    if numbers.is_empty() {
        return Some(while_closed_label().to_string());
    }
    let closed = if while_closed {
        format!("{}{}", tr(", ", "、"), while_closed_label())
    } else {
        String::new()
    };
    Some(if is_chinese() {
        format!("第 {} 轮{closed}", numbers.join("、"))
    } else if numbers.len() == 1 {
        format!("Turn {}{closed}", numbers[0])
    } else {
        format!("Turns {}{closed}", numbers.join(", "))
    })
}

/// SOLO: What changed while Solo was closed, in a terminal that kept
/// running (G59).
fn while_closed_label() -> &'static str {
    tr("While Solo was closed", "Solo 关闭期间")
}


/// SOLO: The version of a file its buffer holds: absent when the file isn't
/// on disk and the buffer is empty.
fn buffer_version(buffer: &Entity<Buffer>, cx: &App) -> Option<Version> {
    let buffer = buffer.read(cx);
    if buffer.is_dirty() || buffer.has_conflict() {
        return None;
    }
    let exists = buffer
        .file()
        .is_some_and(|file| file.disk_state().exists());
    let text = buffer.text();
    if !exists && text.is_empty() {
        Some(Version::Absent)
    } else {
        Some(Version::Text(text.into()))
    }
}

/// SOLO: What to tell about files a rollback left alone because they're in
/// a linked folder (a junction or symlink), or are repositories nested in the
/// work tree.
/// SOLO: Says which large untracked files no snapshot has, so a rollback
/// leaves them as they are (G51).
fn large_files_message(large_files: &[String]) -> Option<SharedString> {
    if large_files.is_empty() {
        return None;
    }
    const MAX_NAMED: usize = 3;
    let mut names = large_files
        .iter()
        .take(MAX_NAMED)
        .cloned()
        .collect::<Vec<_>>();
    if large_files.len() > MAX_NAMED {
        names.push("…".to_string());
    }
    let count = large_files.len();
    let megabytes = MAX_UNTRACKED_FILE_BYTES / (1024 * 1024);
    Some(if is_chinese() {
        format!(
            "{count} 个大文件（超过 {megabytes} MB）没有快照，回滚不会还原它们：{}",
            names.join("、")
        )
        .into()
    } else {
        format!(
            "{count} large files (over {megabytes} MB) have no snapshot. Rolling back won't restore them: {}",
            names.join(", ")
        )
        .into()
    })
}

fn join_messages(messages: impl IntoIterator<Item = Option<SharedString>>) -> Option<SharedString> {
    let messages: Vec<SharedString> = messages.into_iter().flatten().collect();
    (!messages.is_empty()).then(|| {
        let messages: Vec<&str> = messages.iter().map(|message| message.as_ref()).collect();
        messages.join(" ").into()
    })
}

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
            "{} 项在链接的文件夹或嵌套仓库里，没有改动：{}",
            skipped.len(),
            names.join("、")
        )
        .into()
    } else {
        format!(
            "{} items in linked folders or nested repositories were left alone: {}",
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
    // SOLO: all the contents with one git process, not two for each file
    // (R3 L-G9).
    let mut wanted = Vec::new();
    for file in &changed {
        if file.change != FileChange::Added {
            wanted.push((before.to_string(), file.path.clone()));
        }
        if file.change != FileChange::Deleted {
            wanted.push((after.to_string(), file.path.clone()));
        }
    }
    let contents = store
        .files_contents(&wanted)
        .log_err()
        .unwrap_or_else(|| vec![None; wanted.len()]);
    let mut contents = contents.into_iter().map(|bytes| {
        bytes
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .filter(|text| !text.contains('\0'))
    });
    let mut skipped = 0;
    let mut files = Vec::new();
    for file in changed {
        let base = (file.change != FileChange::Added).then(|| contents.next().flatten());
        let after = (file.change != FileChange::Deleted).then(|| contents.next().flatten());
        let (Some(base), Some(after)) = (
            base.map_or(Some(None), |text| text.map(Some)),
            after.map_or(Some(None), |text| text.map(Some)),
        ) else {
            skipped += 1;
            continue;
        };
        let abs_path = store.abs_path(&file.path);
        files.push((file.path, abs_path, base, after));
    }
    (files, skipped)
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

/// SOLO: The message for the agent: the review comments, then what the
/// user rejected, by where it is now (G59).
fn agent_message(comments: &[ReviewComment], rejected: &[Rejection]) -> String {
    let mut message = String::new();
    if !comments.is_empty() {
        message.push_str(&review_message(comments));
    }
    if !rejected.is_empty() {
        if !message.is_empty() {
            message.push('\n');
        }
        message.push_str(tr(
            "I rejected these changes and put them back as they were; don't make them again:\n",
            "我拒绝了下面这些改动，已经恢复原样，不要再改回去：\n",
        ));
        message.push('\n');
        let mut listed: Vec<String> = Vec::new();
        for rejection in rejected {
            let location = match &rejection.lines {
                None => rejection.path.clone(),
                Some(lines) if lines.end <= lines.start + 1 => {
                    format!("{}:{}", rejection.path, lines.start)
                }
                Some(lines) => format!("{}:{}-{}", rejection.path, lines.start, lines.end - 1),
            };
            if !listed.contains(&location) {
                message.push_str(&format!("- {location}\n"));
                listed.push(location);
            }
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

/// SOLO: A file as a turn left it, for its read-only page: named as the
/// file, not opened from disk (G59).
struct TurnFile {
    path: Arc<RelPath>,
    abs_path: PathBuf,
    worktree_id: project::WorktreeId,
    name: String,
}

impl language::File for TurnFile {
    fn as_local(&self) -> Option<&dyn language::LocalFile> {
        None
    }

    fn disk_state(&self) -> language::DiskState {
        language::DiskState::Historic { was_deleted: false }
    }

    fn path_style(&self, _: &App) -> util::paths::PathStyle {
        util::paths::PathStyle::local()
    }

    fn path(&self) -> &Arc<RelPath> {
        &self.path
    }

    fn full_path(&self, _: &App) -> PathBuf {
        self.abs_path.clone()
    }

    fn file_name<'a>(&'a self, _: &'a App) -> &'a str {
        &self.name
    }

    fn worktree_id(&self, _: &App) -> project::WorktreeId {
        self.worktree_id
    }

    fn to_proto(&self, _: &App) -> language::proto::File {
        language::proto::File::default()
    }

    fn is_private(&self) -> bool {
        false
    }

    fn can_open(&self) -> bool {
        false
    }
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
        // SOLO: only the pending changes are kept or rejected (G59).
        if !matches!(review.read(cx).mode, Mode::Pending) {
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
                            "Put this back as you last accepted it",
                            "恢复为你上次认可时的样子",
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
        // SOLO: "Pending Changes · godot_engine", or the turn's (G59).
        let directory = AgentTurns::try_global(cx).and_then(|turns| {
            let turns = turns.read(cx);
            match self.mode {
                Mode::Turn(number) => turns
                    .turn(self.terminal_id, number)
                    .map(|turn| turn.directory.clone()),
                _ => turns
                    .pending_changes(self.terminal_id)
                    .map(|changes| changes.directory.clone())
                    .or_else(|| {
                        turns
                            .turns(self.terminal_id)
                            .last()
                            .map(|turn| turn.directory.clone())
                    }),
            }
        });
        let place = directory
            .as_deref()
            .and_then(Path::file_name)
            .map(|name| name.to_string_lossy().into_owned())
            .or_else(|| {
                self.terminal_view.upgrade().and_then(|view| {
                    view.read(cx)
                        .agent_status()
                        .map(|(name, _)| name.to_string())
                })
            });
        let what = match self.mode {
            Mode::Turn(number) if is_chinese() => format!("第 {number} 轮"),
            Mode::Turn(number) => format!("Turn {number}"),
            _ => tr("Pending Changes", "待审改动").to_string(),
        };
        match place {
            Some(place) => format!("{what} · {place}").into(),
            None => what.into(),
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
        // SOLO: open while it's in a pane (G59).
        if self.pending_page && !self.counted_open.replace(true) {
            let terminal_id = self.terminal_id;
            AgentTurns::global(cx).update(cx, |turns, _| turns.review_opened(terminal_id));
        }
    }

    fn on_removed(&self, cx: &mut Context<Self>) {
        // Counted again if it's added to another pane.
        if self.counted_open.replace(false) {
            close_review(self.terminal_id, cx);
        }
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
        let locked = self.render_locked_files(cx);
        let any_locked = !locked.is_empty();
        v_flex()
            .size_full()
            .track_focus(&self.focus_handle)
            .bg(cx.theme().colors().editor_background)
            .child(self.render_header(cx))
            .children(self.render_diagnostics(cx))
            .children(locked)
            .child(if empty {
                let message = if self.loading {
                    tr("Loading…", "正在加载…")
                } else if matches!(self.mode, Mode::RolledBack { .. }) {
                    ""
                } else if any_locked {
                    tr(
                        "The rest shows once the agent's turn ends.",
                        "这一轮结束后就能审。",
                    )
                } else {
                    tr("No changes left to review.", "没有待审的改动。")
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

    /// SOLO: The comments, then what was rejected, as the design has it
    /// (G59).
    #[test]
    fn test_agent_message() {
        let comments = [ReviewComment {
            path: "runtime/dynamic_bone_3d.cpp".into(),
            lines: 266..=266,
            code: "\t_solve(parameters, p_delta);\n".into(),
            comment: "Why is the main thread still busy?".into(),
        }];
        let rejected = [
            Rejection {
                path: "runtime/collider.cpp".into(),
                lines: Some(45..52),
            },
            Rejection {
                path: "runtime/collider.cpp".into(),
                lines: Some(80..80),
            },
            Rejection {
                path: "new.txt".into(),
                lines: None,
            },
        ];
        let comments_heading = tr(
            "Please address these review comments on your changes:\n",
            "请处理以下针对你的改动的审查意见：\n",
        );
        let rejected_heading = tr(
            "I rejected these changes and put them back as they were; don't make them again:\n",
            "我拒绝了下面这些改动，已经恢复原样，不要再改回去：\n",
        );
        assert_eq!(
            agent_message(&comments, &rejected),
            format!(
                "{comments_heading}\n- runtime/dynamic_bone_3d.cpp:266 — Why is the main thread still busy?\n  ```\n  \t_solve(parameters, p_delta);\n  ```\n\n{rejected_heading}\n- runtime/collider.cpp:45-51\n- runtime/collider.cpp:80\n- new.txt\n"
            )
        );
        // Only rejections.
        assert_eq!(
            agent_message(&[], &rejected[..1]),
            format!("{rejected_heading}\n- runtime/collider.cpp:45-51\n")
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
            let version = |text: Option<&str>| match text {
                Some(text) => Version::Text(text.into()),
                None => Version::Absent,
            };
            files.push(LoadedFile {
                path: name.to_string(),
                abs_path,
                base: base.map(str::to_string),
                buffer,
                // The turn started now: these files aren't on disk, so none
                // was there before it.
                pending: Some(PendingFile {
                    accepted: version(*base),
                    left: version(after.as_deref()),
                    first_turn: 1,
                    since: Some(SystemTime::now()),
                    while_closed: None,
                }),
                turn_versions: Arc::default(),
            });
        }
        let review = cx.new_window_entity(|window, cx| {
            AgentTurnReview::new(terminal_view, project.clone(), false, window, cx)
        });
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.add_item_to_active_pane(Box::new(review.clone()), None, true, window, cx)
        });
        review.update(cx, |review, cx| review.set_files(files, 0, cx));
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

        // SOLO: what the user adds to the file after keeping it is theirs,
        // not the agent's: it isn't pending (G59).
        fs.save(
            path!("/project/b.txt").as_ref(),
            &"b changed\nand more\n".into(),
            language::LineEnding::Unix,
        )
        .await
        .unwrap();
        cx.run_until_parked();
        assert_eq!(hunk_count(&review, cx), 1);

        // Rejecting the rest puts a.txt back, after its first hunk was
        // rejected; b.txt is left alone.
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
        assert_eq!(hunk_count(&review, cx), 0);
        // What was rejected goes to the agent, by where it is now.
        review.read_with(cx, |review, cx| {
            let rejected: Vec<String> = review
                .unsent_rejected(cx)
                .iter()
                .map(|rejection| format!("{}:{:?}", rejection.path, rejection.lines))
                .collect();
            assert_eq!(
                rejected,
                vec![
                    "a.txt:Some(2..3)".to_string(),
                    "c.txt:None".to_string(),
                    "a.txt:Some(10..11)".to_string(),
                ]
            );
        });
    }

    /// A file with a BOM shows only what the turn changed: not its first line
    /// for the BOM (R3 L-G7).
    #[gpui::test]
    async fn test_files_with_a_bom_show_only_their_changes(cx: &mut TestAppContext) {
        let (_fs, review, cx) = open_review(
            cx,
            json!({ "a.txt": "\u{feff}one\r\nTWO\r\nthree\r\n" }),
            &[("a.txt", Some("\u{feff}one\r\ntwo\r\nthree\r\n"))],
        )
        .await;
        let rows = review.update(cx, |review, cx| {
            let snapshot = review.multibuffer.read(cx).snapshot(cx);
            snapshot
                .diff_hunks_in_range(Point::zero()..Point::MAX)
                .map(|hunk| hunk.row_range.start.0)
                .collect::<Vec<_>>()
        });
        assert_eq!(rows.len(), 1, "{rows:?}");
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
            for file in &mut review.files {
                let name = file.abs_path.file_name().unwrap().to_owned();
                file.abs_path = real(&name.to_string_lossy());
                if let Some(pending) = file.pending.as_mut() {
                    pending.since = Some(turn_started_at);
                }
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

    /// SOLO: Rejecting works on files changed after the turn too: what the
    /// user changed that doesn't touch the agent's hunks stays; only a file
    /// with unsaved edits isn't rejected (G59).
    #[gpui::test]
    async fn test_rejecting_after_later_edits(cx: &mut TestAppContext) {
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

        // The user's edit of the agent's line is rejected with it.
        assert_eq!(
            fs.load(path!("/project/edited.txt").as_ref())
                .await
                .unwrap(),
            "one\ntwo\nthree\n"
        );
        // The line the user added to the agent's new file stays.
        assert_eq!(
            fs.load(path!("/project/created.txt").as_ref())
                .await
                .unwrap(),
            "mine\n"
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
            assert!(status.contains("unsaved.txt"), "{status}");
            for name in ["edited.txt", "created.txt", "untouched.txt"] {
                assert!(!status.contains(name), "{status}");
            }
        });
        // The file nobody touched since is rejected.
        assert_eq!(
            fs.load(path!("/project/untouched.txt").as_ref())
                .await
                .unwrap(),
            "before\n"
        );
    }

    /// A file in use stops a rollback partway: the page keeps the way back,
    /// and undoing puts every file back (R3 M2).
    #[gpui::test]
    async fn test_rollback_that_fails_partway_can_be_undone(cx: &mut TestAppContext) {
        init_test(cx);
        cx.executor().allow_parking();
        let temp = tempfile::tempdir().unwrap();
        let repository = temp.path().join("repo");
        std::fs::create_dir_all(repository.join("held")).unwrap();
        #[allow(clippy::disallowed_methods, reason = "test setup")]
        let git = |args: &[&str]| {
            let output = util::command::new_std_command("git")
                .current_dir(&repository)
                .args(["-c", "user.name=Solo", "-c", "user.email=solo@example.com"])
                .args(args)
                .output()
                .unwrap();
            assert!(output.status.success(), "git {args:?}");
        };
        git(&["init", "--quiet"]);
        git(&["config", "core.autocrlf", "false"]);
        std::fs::write(repository.join("a.txt"), "before\n").unwrap();
        std::fs::write(repository.join("held/h.txt"), "before\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "--quiet", "-m", "initial"]);
        let read = |path: &str| std::fs::read_to_string(repository.join(path)).ok();

        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(path!("/project"), json!({})).await;
        let project = Project::test(fs, [path!("/project").as_ref()], cx).await;
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

        // A turn that changes two files and creates one.
        let turns = cx.update(|_, cx| AgentTurns::global(cx));
        turns.update(cx, |turns, cx| {
            turns.storage_root = Some(temp.path().join("storage"));
            turns.start_turn(
                terminal_view.clone(),
                Some(repository.clone()),
                None,
                WeakEntity::new_invalid(),
                Default::default(),
                cx,
            )
        });
        cx.run_until_parked();
        std::fs::write(repository.join("a.txt"), "after\n").unwrap();
        std::fs::write(repository.join("held/h.txt"), "after\n").unwrap();
        std::fs::write(repository.join("new.txt"), "new\n").unwrap();
        turns.update(cx, |turns, cx| {
            turns.end_turn(terminal_view.clone(), None, cx)
        });
        cx.run_until_parked();

        let review = cx.new_window_entity(|window, cx| {
            AgentTurnReview::new(terminal_view.clone(), project.clone(), false, window, cx)
        });
        review.update_in(cx, |review, window, cx| {
            review.set_mode(Mode::ConfirmRollback(1), window, cx)
        });
        cx.run_until_parked();

        // Another program has a file open while rolling back.
        let held = super::super::snapshot::FileInUse::hold(&repository.join("held/h.txt"));
        review.update_in(cx, |review, window, cx| review.confirm_rollback(window, cx));
        cx.run_until_parked();
        assert_eq!(read("a.txt").as_deref(), Some("before\n"));
        assert_eq!(read("new.txt"), None);
        assert_eq!(read("held/h.txt").as_deref(), Some("after\n"));
        review.read_with(cx, |review, _| {
            assert!(
                matches!(
                    review.mode,
                    Mode::RolledBack {
                        stopped_partway: true,
                        ..
                    }
                ),
                "the way back is kept"
            );
            let status = review.status.clone().unwrap_or_default();
            assert!(status.contains("Undo Rollback"), "{status}");
        });

        // Once the file is closed, undoing puts every file back.
        drop(held);
        review.update_in(cx, |review, window, cx| review.undo_rollback(window, cx));
        cx.run_until_parked();
        assert_eq!(read("a.txt").as_deref(), Some("after\n"));
        assert_eq!(read("held/h.txt").as_deref(), Some("after\n"));
        assert_eq!(read("new.txt").as_deref(), Some("new\n"));
        review.read_with(cx, |review, _| {
            assert!(matches!(review.mode, Mode::Pending), "back to the pending changes");
        });
    }

    /// Two agents work in the same repository at once, each on its own file:
    /// each turn is only what its agent reported editing, and rejecting one
    /// leaves the other's file alone (R3 M5). An agent that doesn't report
    /// its edits gets everything that changed, and the page says so.
    #[gpui::test]
    async fn test_agents_in_one_repository_reject_only_their_own_files(cx: &mut TestAppContext) {
        init_test(cx);
        cx.executor().allow_parking();
        let temp = tempfile::tempdir().unwrap();
        let repository = temp.path().join("repo");
        std::fs::create_dir_all(&repository).unwrap();
        #[allow(clippy::disallowed_methods, reason = "test setup")]
        let git = |args: &[&str]| {
            let output = util::command::new_std_command("git")
                .current_dir(&repository)
                .args(["-c", "user.name=Solo", "-c", "user.email=solo@example.com"])
                .args(args)
                .output()
                .unwrap();
            assert!(output.status.success(), "git {args:?}");
        };
        git(&["init", "--quiet"]);
        git(&["config", "core.autocrlf", "false"]);
        for name in ["a.txt", "b.txt"] {
            std::fs::write(repository.join(name), "before\n").unwrap();
        }
        git(&["add", "."]);
        git(&["commit", "--quiet", "-m", "initial"]);
        // The repository as git spells it, which the review uses: the temp
        // directory can be spelled with a short (8.3) name (on CI,
        // C:\Users\RUNNER~1\...), and the fake file system below knows only
        // the spelling it's given.
        #[allow(clippy::disallowed_methods, reason = "test setup")]
        let top_level = util::command::new_std_command("git")
            .current_dir(&repository)
            .args(["rev-parse", "--show-toplevel"])
            .output()
            .unwrap();
        let top_level = String::from_utf8(top_level.stdout).unwrap();
        let repository =
            PathBuf::from(top_level.trim().replace('/', std::path::MAIN_SEPARATOR_STR));

        // The project's files are fake, mirroring the real ones git sees.
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            &repository,
            json!({ "a.txt": "before\n", "b.txt": "before\n" }),
        )
        .await;
        let project = Project::test(fs.clone(), [repository.as_path()], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let new_terminal = |cx: &mut VisualTestContext| {
            workspace.update_in(cx, |workspace, window, cx| {
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
            })
        };
        let first = new_terminal(cx);
        let second = new_terminal(cx);
        let unreported = new_terminal(cx);
        let terminals = [&first, &second, &unreported];

        let turns = cx.update(|_, cx| AgentTurns::global(cx));
        turns.update(cx, |turns, cx| {
            turns.storage_root = Some(temp.path().join("storage"));
            for terminal in terminals {
                turns.start_turn(
                    terminal.clone(),
                    Some(repository.clone()),
                    None,
                    WeakEntity::new_invalid(),
                    Default::default(),
                    cx,
                );
            }
        });
        // git takes the turns' first snapshots on a background thread, which
        // run_until_parked doesn't wait for; slower machines (CI) need longer.
        let started = std::time::Instant::now();
        loop {
            cx.run_until_parked();
            let taken = turns.read_with(cx, |turns, _| {
                terminals
                    .iter()
                    .all(|terminal| turns.start_snapshot_taken(terminal.entity_id()))
            });
            if taken {
                break;
            }
            assert!(
                started.elapsed() < std::time::Duration::from_secs(30),
                "the turns' first snapshots weren't taken"
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        // Each agent edits its file, and its hooks say so; the third one's
        // don't report edits.
        for (name, terminal) in [("a.txt", &first), ("b.txt", &second)] {
            let path = repository.join(name);
            std::fs::write(&path, format!("{name} by its agent\n")).unwrap();
            fs.save(
                &path,
                &format!("{name} by its agent\n").as_str().into(),
                language::LineEnding::Unix,
            )
            .await
            .unwrap();
            // As the hook reports it, through the terminal.
            terminal.update(cx, |_, cx| {
                cx.emit(crate::agent_status::AgentEditedFile(path))
            });
        }
        turns.update(cx, |turns, cx| {
            for terminal in terminals {
                turns.end_turn(terminal.clone(), None, cx);
            }
        });
        cx.run_until_parked();

        let recorded = |terminal: &Entity<TerminalView>, cx: &mut VisualTestContext| {
            turns.read_with(cx, |turns, _| {
                let turn = turns.turn(terminal.entity_id(), 1).cloned().unwrap();
                let paths = |files: &[ChangedFile]| {
                    let mut paths: Vec<String> =
                        files.iter().map(|file| file.path.clone()).collect();
                    paths.sort();
                    paths
                };
                (turn.attributed, paths(&turn.changed), paths(&turn.others))
            })
        };
        assert_eq!(
            recorded(&first, cx),
            (true, vec!["a.txt".to_string()], vec!["b.txt".to_string()])
        );
        assert_eq!(
            recorded(&second, cx),
            (true, vec!["b.txt".to_string()], vec!["a.txt".to_string()])
        );
        assert_eq!(
            recorded(&unreported, cx),
            (
                false,
                vec!["a.txt".to_string(), "b.txt".to_string()],
                vec![]
            )
        );

        // SOLO: a Codex patch's event that came after the turn ended: the
        // file it edited is the turn's after all (R3 M5).
        let pending_of = |terminal: &Entity<TerminalView>, cx: &mut VisualTestContext| {
            turns.read_with(cx, |turns, _| {
                turns
                    .pending_changes(terminal.entity_id())
                    .map(|changes| changes.files.keys().cloned().collect::<Vec<_>>())
                    .unwrap_or_default()
            })
        };
        assert_eq!(pending_of(&second, cx), vec!["b.txt".to_string()]);
        turns.update(cx, |turns, cx| {
            assert!(!turns.late_edits(second.entity_id(), 1, &[repository.join("c.txt")], cx));
            assert!(turns.late_edits(second.entity_id(), 1, &[repository.join("a.txt")], cx));
            assert!(!turns.late_edits(second.entity_id(), 1, &[repository.join("a.txt")], cx));
        });
        assert_eq!(
            recorded(&second, cx),
            (true, vec!["a.txt".to_string(), "b.txt".to_string()], vec![])
        );
        // SOLO: and its changes are pending too (G59).
        let started = std::time::Instant::now();
        while pending_of(&second, cx).len() < 2 {
            assert!(
                started.elapsed() < std::time::Duration::from_secs(30),
                "the late file isn't pending"
            );
            cx.run_until_parked();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(
            pending_of(&second, cx),
            vec!["a.txt".to_string(), "b.txt".to_string()]
        );

        // Rejecting everything in the first agent's turn.
        let review = cx.new_window_entity(|window, cx| {
            AgentTurnReview::new(first.clone(), project.clone(), false, window, cx)
        });
        // The turn's own page says whose the other files are.
        review.update_in(cx, |review, window, cx| {
            review.set_mode(Mode::Turn(1), window, cx)
        });
        cx.run_until_parked();
        review.read_with(cx, |review, cx| {
            assert_eq!(review.files.len(), 1);
            let note = review.attribution_note(cx).unwrap_or_default();
            assert!(note.contains("b.txt"), "{note}");
        });
        // Its pending changes are only its own file.
        review.update_in(cx, |review, window, cx| {
            review.set_mode(Mode::Pending, window, cx)
        });
        cx.run_until_parked();
        review.read_with(cx, |review, _| {
            let paths: Vec<&str> = review.files.iter().map(|file| file.path.as_str()).collect();
            assert_eq!(paths, vec!["a.txt"]);
        });
        review.update(cx, |review, cx| {
            review.reject_hunks(AgentTurnReview::all_hunks(), cx)
        });
        // The restore runs git on a background thread, which run_until_parked
        // doesn't wait for; slower machines (CI) need a moment longer.
        let started = std::time::Instant::now();
        loop {
            cx.run_until_parked();
            let restored = fs.load(&repository.join("a.txt")).await.unwrap() == "before\n";
            if restored || started.elapsed() > std::time::Duration::from_secs(10) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let state = review.read_with(cx, |review, cx| {
            let files: Vec<String> = review
                .files
                .iter()
                .map(|file| {
                    format!(
                        "{} base={} dirty={} abs={:?}",
                        file.path_key.path.as_unix_str(),
                        file.base.is_some(),
                        file.buffer.read(cx).is_dirty(),
                        file.abs_path
                    )
                })
                .collect();
            format!(
                "status={:?} work_tree={:?} files={files:?}",
                review.status, review.work_tree
            )
        });
        assert_eq!(
            fs.load(&repository.join("a.txt")).await.unwrap(),
            "before\n",
            "{state} repository={repository:?}"
        );
        assert_eq!(
            fs.load(&repository.join("b.txt")).await.unwrap(),
            "b.txt by its agent\n"
        );

        // The agent that doesn't report its edits: the page says the files
        // may not all be its own.
        let review = cx.new_window_entity(|window, cx| {
            AgentTurnReview::new(unreported.clone(), project.clone(), false, window, cx)
        });
        review.update_in(cx, |review, window, cx| {
            review.set_mode(Mode::Turn(1), window, cx)
        });
        cx.run_until_parked();
        review.read_with(cx, |review, cx| {
            let note = review.attribution_note(cx).unwrap_or_default();
            assert!(note.contains("may not be this agent's"), "{note}");
        });
    }

    /// SOLO: A git repository, with a fake file system mirroring it for the
    /// project, a workspace and an agent's terminal (G59).
    struct Mirrored<'a> {
        _temp: tempfile::TempDir,
        repository: PathBuf,
        fs: Arc<FakeFs>,
        project: Entity<Project>,
        workspace: Entity<Workspace>,
        terminal_view: Entity<TerminalView>,
        turns: Entity<AgentTurns>,
        cx: &'a mut VisualTestContext,
    }

    async fn mirrored<'a>(cx: &'a mut TestAppContext, files: &[(&str, &str)]) -> Mirrored<'a> {
        init_test(cx);
        cx.executor().allow_parking();
        let temp = tempfile::tempdir().unwrap();
        let repository = temp.path().join("repo");
        std::fs::create_dir_all(&repository).unwrap();
        #[allow(clippy::disallowed_methods, reason = "test setup")]
        let git = |args: &[&str]| {
            let output = util::command::new_std_command("git")
                .current_dir(&repository)
                .args(["-c", "user.name=Solo", "-c", "user.email=solo@example.com"])
                .args(args)
                .output()
                .unwrap();
            assert!(output.status.success(), "git {args:?}");
            output.stdout
        };
        git(&["init", "--quiet"]);
        git(&["config", "core.autocrlf", "false"]);
        for (name, text) in files {
            std::fs::write(repository.join(name), text).unwrap();
        }
        git(&["add", "."]);
        git(&["commit", "--quiet", "-m", "initial"]);
        // As git spells it, which the snapshots use (see above).
        let top_level = String::from_utf8(git(&["rev-parse", "--show-toplevel"])).unwrap();
        let repository =
            PathBuf::from(top_level.trim().replace('/', std::path::MAIN_SEPARATOR_STR));
        let fs = FakeFs::new(cx.executor());
        let tree: serde_json::Map<String, serde_json::Value> = files
            .iter()
            .map(|(name, text)| (name.to_string(), json!(text)))
            .collect();
        fs.insert_tree(&repository, serde_json::Value::Object(tree))
            .await;
        let project = Project::test(fs.clone(), [repository.as_path()], cx).await;
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
        let turns = cx.update(|_, cx| AgentTurns::global(cx));
        turns.update(cx, |turns, _| {
            turns.storage_root = Some(temp.path().join("storage"))
        });
        Mirrored {
            _temp: temp,
            repository,
            fs,
            project,
            workspace,
            terminal_view,
            turns,
            cx,
        }
    }

    impl Mirrored<'_> {
        /// The agent (or the user) writes a file: on disk for git, and in the
        /// project's file system.
        async fn write(&mut self, name: &str, text: &str) {
            let path = self.repository.join(name);
            std::fs::write(&path, text).unwrap();
            if self.fs.is_file(&path).await {
                self.fs
                    .save(&path, &text.into(), language::LineEnding::Unix)
                    .await
                    .unwrap();
            } else {
                self.fs.insert_file(&path, text.as_bytes().to_vec()).await;
            }
            self.cx.run_until_parked();
        }

        /// SOLO: The agent writes a file's bytes, line breaks and all.
        async fn write_bytes(&mut self, name: &str, bytes: &[u8]) {
            let path = self.repository.join(name);
            std::fs::write(&path, bytes).unwrap();
            self.fs.write(&path, bytes).await.unwrap();
            self.cx.run_until_parked();
        }

        /// What the review saved goes to disk, for git.
        async fn sync_to_disk(&mut self, name: &str) {
            let path = self.repository.join(name);
            let text = self.fs.load(&path).await.unwrap();
            std::fs::write(&path, text).unwrap();
        }

        fn wait(&mut self, what: &str, done: impl Fn(&AgentTurns) -> bool) {
            let started = std::time::Instant::now();
            loop {
                self.cx.run_until_parked();
                if self.turns.read_with(self.cx, |turns, _| done(turns)) {
                    return;
                }
                assert!(
                    started.elapsed() < std::time::Duration::from_secs(30),
                    "timed out waiting for {what}"
                );
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }

        fn start_turn(&mut self) {
            let terminal_view = self.terminal_view.clone();
            let repository = self.repository.clone();
            self.turns.update(self.cx, |turns, cx| {
                turns.start_turn(
                    terminal_view,
                    Some(repository),
                    None,
                    WeakEntity::new_invalid(),
                    Default::default(),
                    cx,
                )
            });
            let terminal_id = self.terminal_view.entity_id();
            self.wait("the turn's first snapshot", |turns| {
                turns.start_snapshot_taken(terminal_id)
            });
        }

        fn end_turn(&mut self) {
            let terminal_view = self.terminal_view.clone();
            self.turns
                .update(self.cx, |turns, cx| turns.end_turn(terminal_view, None, cx));
            let terminal_id = self.terminal_view.entity_id();
            self.wait("the turn's end", |turns| turns.active_turns(terminal_id).is_empty());
        }

        fn pending(&mut self) -> Vec<String> {
            let terminal_id = self.terminal_view.entity_id();
            self.turns.read_with(self.cx, |turns, _| {
                turns
                    .pending_changes(terminal_id)
                    .map(|changes| changes.pending.iter().cloned().collect())
                    .unwrap_or_default()
            })
        }

        fn open_review(&mut self) -> Entity<AgentTurnReview> {
            let terminal_view = self.terminal_view.clone();
            let project = self.project.clone();
            let review = self.cx.new_window_entity(|window, cx| {
                AgentTurnReview::new(terminal_view, project, true, window, cx)
            });
            self.workspace.update_in(self.cx, |workspace, window, cx| {
                workspace.add_item_to_active_pane(Box::new(review.clone()), None, true, window, cx)
            });
            self.cx.run_until_parked();
            review
        }

        /// Closes the review's tab.
        fn close_review(&mut self, review: Entity<AgentTurnReview>) {
            let item_id = review.entity_id();
            drop(review);
            self.workspace.update_in(self.cx, |workspace, window, cx| {
                workspace.active_pane().update(cx, |pane, cx| {
                    pane.remove_item(item_id, false, false, window, cx)
                })
            });
            self.cx.run_until_parked();
        }
    }

    fn ten_lines(changed: &[(usize, &str)]) -> String {
        (1..=10)
            .map(|row| {
                let line = changed
                    .iter()
                    .find(|(changed_row, _)| *changed_row == row)
                    .map(|(_, text)| text.to_string())
                    .unwrap_or_else(|| format!("line {row}"));
                line + "\n"
            })
            .collect()
    }

    /// The files shown, and the files locked.
    fn shown(review: &Entity<AgentTurnReview>, cx: &mut VisualTestContext) -> (Vec<String>, Vec<String>) {
        review.read_with(cx, |review, cx| {
            let snapshot = review.multibuffer.read(cx).snapshot(cx);
            let mut shown: Vec<String> = snapshot
                .diff_hunks_in_range(Point::zero()..Point::MAX)
                .filter_map(|hunk| {
                    let ix = review.file_index(hunk.buffer_id, cx)?;
                    Some(review.files[ix].path.clone())
                })
                .collect();
            shown.dedup();
            (shown, review.locked.clone())
        })
    }

    /// SOLO: The pending changes across turns (G59): the same file changed
    /// in two turns; while the second runs, its files are locked and the
    /// others can be reviewed; after it, a hunk of the first rejects, and
    /// each hunk says its turn; what only the first changed is accepted once
    /// the review closes (not while it's open), and the rest at the next
    /// turn's end.
    #[gpui::test]
    async fn test_pending_changes_across_turns(cx: &mut TestAppContext) {
        let mut test = mirrored(cx, &[("a.txt", &ten_lines(&[])), ("c.txt", "c\n")]).await;
        let terminal_id = test.terminal_view.entity_id();

        // Turn 1 changes a.txt's second line, and c.txt.
        test.start_turn();
        test.write("a.txt", &ten_lines(&[(2, "TWO")])).await;
        test.write("c.txt", "C\n").await;
        test.end_turn();
        assert_eq!(test.pending(), vec!["a.txt", "c.txt"]);
        // The tab offers them.
        let has_entry = test.terminal_view.read_with(test.cx, |view, cx| {
            crate::agent_turns::render_tab_entry(view, cx).is_some()
        });
        assert!(has_entry);

        let review = test.open_review();
        assert_eq!(
            shown(&review, test.cx),
            (vec!["a.txt".to_string(), "c.txt".to_string()], vec![])
        );

        // Turn 2 changes a.txt's ninth line, and creates b.txt. While it
        // runs, a.txt is locked; c.txt can be reviewed.
        test.start_turn();
        test.write("a.txt", &ten_lines(&[(2, "TWO"), (9, "NINE")]))
            .await;
        test.write("b.txt", "b\n").await;
        assert_eq!(
            shown(&review, test.cx),
            (vec!["c.txt".to_string()], vec!["a.txt".to_string()])
        );
        test.end_turn();
        test.cx.run_until_parked();
        assert_eq!(
            shown(&review, test.cx),
            (
                vec!["a.txt".to_string(), "b.txt".to_string(), "c.txt".to_string()],
                vec![]
            )
        );
        // The review is open: turn 1's changes are still pending.
        assert_eq!(test.pending(), vec!["a.txt", "b.txt", "c.txt"]);

        // Each hunk of a.txt says its turn; the page filters by them.
        let hunks = review.read_with(test.cx, |review, cx| {
            let snapshot = review.multibuffer.read(cx).snapshot(cx);
            snapshot
                .diff_hunks_in_range(Point::zero()..Point::MAX)
                .filter_map(|hunk| {
                    let ix = review.file_index(hunk.buffer_id, cx)?;
                    (review.files[ix].path == "a.txt").then(|| {
                        let rows = hunk.buffer_range.to_point(&review.files[ix].buffer.read(cx).snapshot());
                        let rows = point_rows(&rows);
                        let turns: Vec<usize> =
                            review.hunk_turns(ix, &rows).into_iter().collect();
                        (rows.start, turns, hunk.multi_buffer_range)
                    })
                })
                .collect::<Vec<_>>()
        });
        assert_eq!(
            hunks
                .iter()
                .map(|(row, turns, _)| (*row, turns.clone()))
                .collect::<Vec<_>>(),
            vec![(1, vec![1]), (8, vec![2])]
        );
        review.read_with(test.cx, |review, cx| {
            assert_eq!(
                review.turn_sources(cx),
                vec![TurnFilter::Turn(1), TurnFilter::Turn(2)]
            );
        });
        review.update(test.cx, |review, cx| review.set_filter(TurnFilter::Turn(2), cx));
        assert_eq!(
            shown(&review, test.cx).0,
            vec!["a.txt".to_string(), "b.txt".to_string()]
        );
        review.update(test.cx, |review, cx| review.set_filter(TurnFilter::All, cx));

        // Viewing a turn opens a page of its own: the pending changes' page
        // (and the comments in it) stays as it is.
        review.update_in(test.cx, |review, window, cx| {
            review.open_turn(Mode::Turn(1), window, cx)
        });
        test.cx.run_until_parked();
        let pages = test.workspace.read_with(test.cx, |workspace, cx| {
            workspace
                .items_of_type::<AgentTurnReview>(cx)
                .map(|page| {
                    let page = page.read(cx);
                    (page.pending_page, matches!(page.mode, Mode::Turn(1)))
                })
                .collect::<Vec<_>>()
        });
        assert_eq!(pages.len(), 2, "{pages:?}");
        assert!(pages.contains(&(false, true)), "{pages:?}");
        review.read_with(test.cx, |review, _| {
            assert!(matches!(review.mode, Mode::Pending));
            assert_eq!(review.files.len(), 3);
        });
        // Rejecting turn 1's hunk works, and leaves turn 2's.
        let first = hunks[0].2.clone();
        review.update(test.cx, |review, cx| review.reject_hunks(vec![first], cx));
        test.cx.run_until_parked();
        assert_eq!(
            test.fs.load(&test.repository.join("a.txt")).await.unwrap(),
            ten_lines(&[(9, "NINE")])
        );
        test.sync_to_disk("a.txt").await;
        // The message for the agent says what was rejected.
        let message = review.read_with(test.cx, |review, cx| {
            agent_message(&[], &review.unsent_rejected(cx))
        });
        assert!(message.ends_with("- a.txt:2\n"), "{message}");

        // Closing the review: what only turn 1 changed (c.txt) is accepted.
        test.close_review(review);
        test.wait("the changes accepted", |turns| {
            turns
                .pending_changes(terminal_id)
                .is_some_and(|changes| !changes.files.contains_key("c.txt"))
        });
        assert_eq!(test.pending(), vec!["a.txt", "b.txt"]);

        // Turn 3 changes nothing: at its end, the rest is accepted.
        test.start_turn();
        test.end_turn();
        test.cx.run_until_parked();
        assert_eq!(test.pending(), Vec::<String>::new());
        let has_entry = test.terminal_view.read_with(test.cx, |view, cx| {
            crate::agent_turns::render_tab_entry(view, cx).is_some()
        });
        assert!(!has_entry);
    }

    /// SOLO: Filtered by a turn, Reject All rejects only that turn's hunks,
    /// even when another turn's hunk is close enough to show in its
    /// context (G59).
    #[gpui::test]
    async fn test_reject_all_rejects_only_the_turn_shown(cx: &mut TestAppContext) {
        let mut test = mirrored(cx, &[("a.txt", &ten_lines(&[]))]).await;
        let review = test.open_review();
        test.start_turn();
        test.write("a.txt", &ten_lines(&[(2, "TWO")])).await;
        test.end_turn();
        test.start_turn();
        test.write("a.txt", &ten_lines(&[(2, "TWO"), (5, "FIVE")]))
            .await;
        test.end_turn();
        test.cx.run_until_parked();
        review.read_with(test.cx, |review, cx| {
            assert_eq!(
                review.turn_sources(cx),
                vec![TurnFilter::Turn(1), TurnFilter::Turn(2)]
            );
        });
        review.update(test.cx, |review, cx| {
            review.set_filter(TurnFilter::Turn(2), cx);
            review.reject_hunks(AgentTurnReview::all_hunks(), cx);
        });
        test.cx.run_until_parked();
        assert_eq!(
            test.fs.load(&test.repository.join("a.txt")).await.unwrap(),
            ten_lines(&[(2, "TWO")])
        );
    }

    /// SOLO: Rolling back accepts the files as it left them; undoing it
    /// puts back what was pending (G59).
    #[gpui::test]
    async fn test_rollback_accepts_what_it_restored(cx: &mut TestAppContext) {
        let mut test = mirrored(cx, &[("a.txt", "before\n")]).await;
        test.start_turn();
        test.write("a.txt", "after\n").await;
        test.end_turn();
        assert_eq!(test.pending(), vec!["a.txt"]);

        let review = test.open_review();
        review.update_in(test.cx, |review, window, cx| {
            review.set_mode(Mode::ConfirmRollback(1), window, cx)
        });
        test.cx.run_until_parked();
        review.update_in(test.cx, |review, window, cx| review.confirm_rollback(window, cx));
        let terminal_id = test.terminal_view.entity_id();
        test.wait("the rollback", |turns| {
            turns
                .pending_changes(terminal_id)
                .is_some_and(|changes| changes.files.is_empty())
        });
        assert_eq!(
            std::fs::read_to_string(test.repository.join("a.txt")).unwrap(),
            "before\n"
        );
        assert_eq!(test.pending(), Vec::<String>::new());

        review.update_in(test.cx, |review, window, cx| review.undo_rollback(window, cx));
        test.wait("the undo", |turns| turns.pending_file_count(terminal_id) == 1);
        assert_eq!(
            std::fs::read_to_string(test.repository.join("a.txt")).unwrap(),
            "after\n"
        );
    }

    /// SOLO: The hunks shown of the file `name`.
    fn hunks_of(
        review: &Entity<AgentTurnReview>,
        name: &str,
        cx: &mut VisualTestContext,
    ) -> Vec<Range<editor::Anchor>> {
        review.read_with(cx, |review, cx| {
            let snapshot = review.multibuffer.read(cx).snapshot(cx);
            snapshot
                .diff_hunks_in_range(Point::zero()..Point::MAX)
                .filter(|hunk| {
                    review
                        .file_index(hunk.buffer_id, cx)
                        .is_some_and(|ix| review.files[ix].path == name)
                })
                .map(|hunk| hunk.multi_buffer_range)
                .collect()
        })
    }

    /// SOLO: Rejecting puts the file back byte for byte: the lines it
    /// doesn't touch keep their line breaks and the byte order mark, and the
    /// lines it puts back have theirs; rejecting all of a file gives back
    /// what was accepted. A file with mixed line breaks used to get the
    /// buffer's one line break on every line (G59).
    #[gpui::test]
    async fn test_rejecting_keeps_line_breaks(cx: &mut TestAppContext) {
        let crlf = "one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix\r\nseven\r\neight\r\n";
        let bom = "\u{feff}one\r\ntwo\r\nthree\r\n";
        let mixed = "# Title\r\nThis is the project.\n\r\n";
        let deleted = "a\r\nb\nc\r\nd\r\ne\r\nf\r\ng\r\nh\r\ni\r\n";
        let lf = "one\ntwo\nthree\n";
        let mut test = mirrored(
            cx,
            &[
                ("crlf.txt", crlf),
                ("bom.txt", bom),
                ("mixed.md", mixed),
                ("deleted.txt", deleted),
                ("lf.txt", lf),
            ],
        )
        .await;
        test.start_turn();
        test.write_bytes(
            "crlf.txt",
            b"one\r\ntwo\r\nADDED\r\nthree\r\nfour\r\nfive\r\nsix\r\nseven\r\nEIGHT\r\n",
        )
        .await;
        test.write_bytes("bom.txt", "\u{feff}one\r\nTWO\r\nthree\r\n".as_bytes())
            .await;
        test.write_bytes(
            "mixed.md",
            b"# Title\r\nThis is the project.\nAgent line\r\n\r\n",
        )
        .await;
        test.write_bytes("deleted.txt", b"a\r\nc\r\nd\r\ne\r\nf\r\ng\r\nh\r\nI\r\n")
            .await;
        test.write_bytes("lf.txt", b"one\nTWO\nthree\n").await;
        test.end_turn();
        let review = test.open_review();
        test.cx.run_until_parked();

        // One hunk of two: the other stays as the agent left it, in CRLF.
        let crlf_hunks = hunks_of(&review, "crlf.txt", test.cx);
        assert_eq!(crlf_hunks.len(), 2);
        // A line put back keeps its own line break.
        let deleted_hunks = hunks_of(&review, "deleted.txt", test.cx);
        assert_eq!(deleted_hunks.len(), 2);
        let mut rejected = vec![crlf_hunks[0].clone(), deleted_hunks[0].clone()];
        for name in ["bom.txt", "mixed.md", "lf.txt"] {
            rejected.extend(hunks_of(&review, name, test.cx));
        }
        review.update(test.cx, |review, cx| review.reject_hunks(rejected, cx));
        test.cx.run_until_parked();

        let fs = test.fs.clone();
        let repository = test.repository.clone();
        let bytes = |name: &str| {
            let fs = fs.clone();
            let path = repository.join(name);
            async move { String::from_utf8(fs.load_bytes(&path).await.unwrap()).unwrap() }
        };
        assert_eq!(
            bytes("crlf.txt").await,
            "one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix\r\nseven\r\nEIGHT\r\n"
        );
        assert_eq!(bytes("bom.txt").await, bom);
        assert_eq!(bytes("mixed.md").await, mixed);
        assert_eq!(
            bytes("deleted.txt").await,
            "a\r\nb\nc\r\nd\r\ne\r\nf\r\ng\r\nh\r\nI\r\n"
        );
        assert_eq!(bytes("lf.txt").await, lf);
        // Saved, as far as the buffers know; what's left to review is the
        // hunks not rejected.
        review.read_with(test.cx, |review, cx| {
            for file in &review.files {
                let buffer = file.buffer.read(cx);
                assert!(
                    !buffer.is_dirty() && !buffer.has_conflict(),
                    "{}",
                    file.path
                );
            }
        });
        let mut pending = test.pending();
        pending.sort();
        assert_eq!(pending, vec!["crlf.txt", "deleted.txt"]);
    }

    /// SOLO: Rejecting a file that changed on disk since the page showed it
    /// changes nothing (not the buffer, the page or the pending changes) and
    /// says why; once the page has the change, rejecting works (G59).
    #[gpui::test]
    async fn test_rejecting_a_file_changed_on_disk(cx: &mut TestAppContext) {
        let mut test = mirrored(cx, &[("a.txt", &ten_lines(&[]))]).await;
        test.start_turn();
        test.write("a.txt", &ten_lines(&[(2, "TWO")])).await;
        test.end_turn();
        let review = test.open_review();
        let path = test.repository.join("a.txt");
        let terminal_id = test.terminal_view.entity_id();
        let pending_before = test.turns.read_with(test.cx, |turns, _| {
            turns.pending_changes(terminal_id).unwrap().files["a.txt"].clone()
        });

        // Someone changes it, and the page doesn't know yet.
        test.fs.pause_events();
        let changed = ten_lines(&[(2, "TWO"), (9, "nine, by someone")]);
        test.fs.write(&path, changed.as_bytes()).await.unwrap();
        let hunks = hunks_of(&review, "a.txt", test.cx);
        review.update(test.cx, |review, cx| review.reject_hunks(hunks, cx));
        test.cx.run_until_parked();
        assert_eq!(test.fs.load(&path).await.unwrap(), changed);
        review.read_with(test.cx, |review, cx| {
            let status = review.status.clone().unwrap_or_default();
            assert!(status.contains("a.txt"), "{status}");
            let buffer = review.files[0].buffer.read(cx);
            assert!(!buffer.is_dirty() && !buffer.has_conflict());
            assert_eq!(buffer.text(), ten_lines(&[(2, "TWO")]));
            assert!(review.unsent_rejected(cx).is_empty());
        });
        assert_eq!(hunks_of(&review, "a.txt", test.cx).len(), 1);
        let pending_after = test.turns.read_with(test.cx, |turns, _| {
            turns.pending_changes(terminal_id).unwrap().files["a.txt"].clone()
        });
        assert_eq!(pending_after, pending_before);

        // The page gets the change; rejecting again works, and leaves it.
        test.fs.unpause_events_and_flush();
        test.cx.run_until_parked();
        std::fs::write(&path, &changed).unwrap();
        let hunks = hunks_of(&review, "a.txt", test.cx);
        assert_eq!(hunks.len(), 1);
        review.update(test.cx, |review, cx| review.reject_hunks(hunks, cx));
        test.cx.run_until_parked();
        assert_eq!(
            test.fs.load(&path).await.unwrap(),
            ten_lines(&[(9, "nine, by someone")])
        );
        review.read_with(test.cx, |review, cx| {
            assert_eq!(review.unsent_rejected(cx).len(), 1);
        });
    }

    /// SOLO: Rejecting hunks of a file one right after the other: each
    /// starts from what the one before left, so all of them are rejected
    /// (not only the last one written), each once, also one rejected twice
    /// (G59).
    #[gpui::test]
    async fn test_rejecting_hunks_of_a_file_at_once(cx: &mut TestAppContext) {
        let mut test = mirrored(cx, &[("a.txt", &ten_lines(&[]))]).await;
        test.start_turn();
        test.write("a.txt", &ten_lines(&[(2, "TWO"), (9, "NINE")]))
            .await;
        test.end_turn();
        let review = test.open_review();
        let hunks = hunks_of(&review, "a.txt", test.cx);
        assert_eq!(hunks.len(), 2);
        review.update(test.cx, |review, cx| {
            review.reject_hunks(vec![hunks[0].clone()], cx);
            review.reject_hunks(vec![hunks[1].clone()], cx);
            review.reject_hunks(vec![hunks[0].clone()], cx);
        });
        test.cx.run_until_parked();
        let path = test.repository.join("a.txt");
        assert_eq!(test.fs.load(&path).await.unwrap(), ten_lines(&[]));
        review.read_with(test.cx, |review, cx| {
            let buffer = review.files[0].buffer.read(cx);
            assert_eq!(buffer.text(), ten_lines(&[]));
            assert!(!buffer.is_dirty() && !buffer.has_conflict());
            let rejected: Vec<String> = review
                .unsent_rejected(cx)
                .iter()
                .map(|rejection| format!("{}:{:?}", rejection.path, rejection.lines))
                .collect();
            assert_eq!(rejected, vec!["a.txt:Some(2..3)", "a.txt:Some(9..10)"]);
        });
        assert!(hunks_of(&review, "a.txt", test.cx).is_empty());
    }

    /// SOLO: A hunk where the agent deleted lines, rejected twice in a row
    /// (a double click, say): the lines come back once, and the agent is
    /// told once (G59).
    #[gpui::test]
    async fn test_rejecting_a_deletion_twice(cx: &mut TestAppContext) {
        let mut test = mirrored(cx, &[("a.txt", &ten_lines(&[]))]).await;
        test.start_turn();
        let deleted: String = ten_lines(&[])
            .lines()
            .filter(|line| *line != "line 5")
            .map(|line| format!("{line}\n"))
            .collect();
        test.write("a.txt", &deleted).await;
        test.end_turn();
        let review = test.open_review();
        let hunks = hunks_of(&review, "a.txt", test.cx);
        assert_eq!(hunks.len(), 1);
        review.update(test.cx, |review, cx| {
            review.reject_hunks(hunks.clone(), cx);
            review.reject_hunks(hunks.clone(), cx);
        });
        test.cx.run_until_parked();
        let path = test.repository.join("a.txt");
        assert_eq!(test.fs.load(&path).await.unwrap(), ten_lines(&[]));
        review.read_with(test.cx, |review, cx| {
            assert_eq!(review.files[0].buffer.read(cx).text(), ten_lines(&[]));
            assert_eq!(review.unsent_rejected(cx).len(), 1);
        });
    }

    /// SOLO: A hunk kept while another hunk of the file is being rejected
    /// stays kept (G59).
    #[gpui::test]
    async fn test_keeping_while_rejecting(cx: &mut TestAppContext) {
        let mut test = mirrored(cx, &[("a.txt", &ten_lines(&[]))]).await;
        let terminal_id = test.terminal_view.entity_id();
        test.start_turn();
        test.write("a.txt", &ten_lines(&[(2, "TWO"), (9, "NINE")]))
            .await;
        test.end_turn();
        let review = test.open_review();
        let hunks = hunks_of(&review, "a.txt", test.cx);
        assert_eq!(hunks.len(), 2);
        let keep = hunks[1].clone();
        review.update(test.cx, |review, cx| {
            // Kept while the rejected file is being written.
            review.after_write = Some(Box::new(move |review, cx| {
                review.keep_hunks(vec![keep], cx);
            }));
            review.reject_hunks(vec![hunks[0].clone()], cx);
        });
        test.cx.run_until_parked();
        let path = test.repository.join("a.txt");
        assert_eq!(
            test.fs.load(&path).await.unwrap(),
            ten_lines(&[(9, "NINE")])
        );
        // Nothing's left to review: line 9 was kept, line 2 rejected.
        let accepted = Version::Text(ten_lines(&[(9, "NINE")]).into());
        review.read_with(test.cx, |review, _| {
            let pending = review.files[0].pending.clone().unwrap();
            assert_eq!(pending.accepted, accepted);
            assert_eq!(pending.left, accepted);
        });
        test.turns.read_with(test.cx, |turns, _| {
            let changes = turns.pending_changes(terminal_id).unwrap();
            assert!(
                !changes.files.contains_key("a.txt"),
                "{:?}",
                changes.files.get("a.txt")
            );
        });
        assert!(hunks_of(&review, "a.txt", test.cx).is_empty());
    }

    /// SOLO: Putting back comments from before doesn't close the overlay
    /// where the user is writing a comment (G59).
    #[gpui::test]
    async fn test_restored_comments_keep_a_draft(cx: &mut TestAppContext) {
        let mut test = mirrored(
            cx,
            &[("a.txt", &ten_lines(&[])), ("b.txt", &ten_lines(&[]))],
        )
        .await;
        test.start_turn();
        test.write("a.txt", &ten_lines(&[(2, "TWO")])).await;
        test.write("b.txt", &ten_lines(&[(5, "FIVE")])).await;
        test.end_turn();
        let review = test.open_review();
        review.update_in(test.cx, |review, window, cx| {
            let row_of = |review: &AgentTurnReview, name: &str, row: u32, cx: &App| {
                let file = review.files.iter().find(|file| file.path == name).unwrap();
                let buffer = file.buffer.read(cx).snapshot();
                let snapshot = review.multibuffer.read(cx).snapshot(cx);
                let start = snapshot
                    .anchor_in_excerpt(buffer.anchor_after(Point::new(row, 0)))
                    .unwrap();
                let end = snapshot
                    .anchor_in_excerpt(buffer.anchor_before(Point::new(row, buffer.line_len(row))))
                    .unwrap();
                start..end
            };
            let draft = row_of(review, "a.txt", 1, cx);
            let restored = row_of(review, "b.txt", 4, cx);
            review.editor.update(cx, |editor, cx| {
                let snapshot = editor.snapshot(window, cx);
                let multibuffer = editor.buffer().read(cx).snapshot(cx);
                let point = draft.start.to_point(&multibuffer);
                let row = snapshot
                    .display_snapshot
                    .point_to_display_point(point, language::Bias::Left)
                    .row();
                // The user starts writing a comment on a.txt.
                editor.show_diff_review_overlay(row..row, window, cx);
                assert_eq!(editor.diff_review_overlay_count(), 1);
                editor.restore_review_comment(restored, "Why five?".to_string(), window, cx);
                assert_eq!(editor.diff_review_overlay_count(), 2);
            });
        });
    }

    /// SOLO: A comment from before is found where its code is now, in all of
    /// the file: also when it was further down than the file now goes.
    #[test]
    fn test_comment_rows() {
        let comment = |lines: std::ops::RangeInclusive<u32>, code: &str| ReviewComment {
            path: "a.txt".into(),
            lines,
            code: code.into(),
            comment: "?".into(),
        };
        let text = "one\ntwo\nthree\nfour\n";
        assert_eq!(
            comment_rows(text, &comment(2..=3, "two\nthree")),
            Some(1..2)
        );
        // Moved up, past the end of the file now.
        assert_eq!(comment_rows(text, &comment(40..=40, "four")), Some(3..3));
        // The nearest of two.
        let text = "x\na\nx\nx\nx\na\n";
        assert_eq!(comment_rows(text, &comment(5..=5, "a")), Some(5..5));
        assert_eq!(comment_rows(text, &comment(1..=1, "a")), Some(1..1));
        assert_eq!(comment_rows(text, &comment(1..=1, "gone")), None);
    }

    /// SOLO: After a restart (or reloading the workspace), the review still
    /// has the turn its pending changes are from, and what wasn't sent to
    /// the agent yet: the hunks rejected and the comments, which then go to
    /// the same agent once, and not again after the next restart (G59).
    #[gpui::test]
    async fn test_review_after_a_restart(cx: &mut TestAppContext) {
        const KEY: &str = "t7654321";
        let mut test = mirrored(
            cx,
            &[("a.txt", &ten_lines(&[])), ("b.txt", &ten_lines(&[]))],
        )
        .await;
        let terminal_id = test.terminal_view.entity_id();
        test.turns.update(test.cx, |turns, _| {
            // As its shell's SOLO_TERMINAL_ID would.
            turns.terminals.entry(terminal_id).or_default().key = Some(KEY.to_string());
        });
        test.start_turn();
        test.write("a.txt", &ten_lines(&[(2, "TWO")])).await;
        test.write("b.txt", &ten_lines(&[(5, "FIVE")])).await;
        test.end_turn();

        // The user rejects a.txt's change and comments on b.txt's.
        let review = test.open_review();
        let a_hunks = hunks_of(&review, "a.txt", test.cx);
        review.update(test.cx, |review, cx| review.reject_hunks(a_hunks, cx));
        test.cx.run_until_parked();
        test.sync_to_disk("a.txt").await;
        // On b.txt's fifth line, as the gutter's button there gives it.
        let (start, end) = review.update(test.cx, |review, cx| {
            let file = review
                .files
                .iter()
                .find(|file| file.path == "b.txt")
                .unwrap();
            let buffer = file.buffer.read(cx).snapshot();
            let snapshot = review.multibuffer.read(cx).snapshot(cx);
            let start = snapshot
                .anchor_in_excerpt(buffer.anchor_after(Point::new(4, 0)))
                .unwrap();
            let end = snapshot
                .anchor_in_excerpt(buffer.anchor_before(Point::new(4, buffer.line_len(4))))
                .unwrap();
            (start, end)
        });
        // As writing it in the overlay there does.
        review.update_in(test.cx, |review, window, cx| {
            review.editor.update(cx, |editor, cx| {
                editor.restore_review_comment(start..end, "Why five?".to_string(), window, cx)
            })
        });
        test.cx.run_until_parked();
        let label = |review: &Entity<AgentTurnReview>, cx: &mut VisualTestContext| {
            review.read_with(cx, |review, cx| review.send_label(cx))
        };
        let both = tr(
            "Send to Agent (1 comment, 1 rejection)",
            "发送给 agent（1 条批注、拒绝 1 处）",
        );
        assert_eq!(label(&review, test.cx), both);
        let unsent = test
            .turns
            .read_with(test.cx, |turns, _| turns.unsent(terminal_id));
        assert_eq!(
            (unsent.rejected.len(), unsent.comments.len()),
            (1, 1),
            "{unsent:?}"
        );
        test.close_review(review);
        test.cx
            .executor()
            .advance_clock(std::time::Duration::from_secs(1));
        test.cx.run_until_parked();

        // Solo restarts: the terminal's view comes back with its id.
        let storage = test
            .turns
            .read_with(test.cx, |turns, _| turns.storage_root());
        let restart = |test: &mut Mirrored| {
            let restarted = test.cx.update(|_, cx| {
                let restarted = cx.new(|_| AgentTurns {
                    storage_root: Some(storage.clone()),
                    ..AgentTurns::default()
                });
                cx.set_global(super::super::GlobalAgentTurns(restarted.clone()));
                restarted
            });
            let terminal_view = test.terminal_view.clone();
            restarted.update(test.cx, |turns, cx| {
                turns.register_terminal(
                    terminal_view,
                    Some(KEY.to_string()),
                    WeakEntity::new_invalid(),
                    cx,
                )
            });
            test.turns = restarted;
        };
        restart(&mut test);
        test.wait("the restore", |turns| {
            turns.has_something_to_review(terminal_id)
        });
        test.cx
            .executor()
            .advance_clock(std::time::Duration::from_secs(1));
        test.cx.run_until_parked();

        let unsent = test
            .turns
            .read_with(test.cx, |turns, _| turns.unsent(terminal_id));
        assert_eq!(
            (unsent.rejected.len(), unsent.comments.len()),
            (1, 1),
            "{unsent:?}"
        );
        let review = test.open_review();
        test.cx.run_until_parked();
        review.read_with(test.cx, |review, cx| {
            // The turn is there, and the page counts it.
            let turns = AgentTurns::try_global(cx).unwrap().read(cx);
            assert_eq!(turns.turns(terminal_id).count(), 1);
            // The comment is back on its line.
            assert!(review.carried.is_empty(), "{:?}", review.carried);
            let comments = review.review_comments(review.editor.read(cx).review_comments(), cx);
            assert_eq!(comments.len(), 1);
            assert_eq!(comments[0].lines, 5..=5);
            assert_eq!(comments[0].comment, "Why five?");
            // Shown, in its review overlay, as after writing it.
            assert_eq!(review.editor.read(cx).diff_review_overlay_count(), 1);
        });
        assert_eq!(label(&review, test.cx), both);
        assert_eq!(hunks_of(&review, "b.txt", test.cx).len(), 1);

        // The agent isn't running: the message is only copied, and what it
        // has is still there to send.
        review.update_in(test.cx, |review, window, cx| {
            review.send_to_agent(window, cx)
        });
        test.cx.run_until_parked();
        assert_eq!(label(&review, test.cx), both);
        let unsent = test
            .turns
            .read_with(test.cx, |turns, _| turns.unsent(terminal_id));
        assert_eq!((unsent.rejected.len(), unsent.comments.len()), (1, 1));

        // It runs, and takes pastes: sent, then there's nothing to send.
        test.terminal_view.update(test.cx, |view, cx| {
            view.terminal.update(cx, |terminal, cx| {
                terminal.write_raw_output(b"\x1b[?2004h", cx)
            });
        });
        test.cx.run_until_parked();
        test.terminal_view.update(test.cx, |view, cx| {
            view.set_agent_status_for_test("claude", crate::agent_status::AgentStatus::Idle);
            assert!(crate::agent_context::can_paste_to_agent(view, "a\nb", cx));
        });
        review.update_in(test.cx, |review, window, cx| {
            review.send_to_agent(window, cx)
        });
        test.cx.run_until_parked();
        review.read_with(test.cx, |review, cx| {
            assert!(review.unsent_rejected(cx).is_empty());
            assert_eq!(review.comment_count + review.carried.len(), 0);
            let turns = AgentTurns::try_global(cx).unwrap().read(cx);
            assert!(turns.unsent(terminal_id).is_empty());
        });
        test.close_review(review);
        test.cx
            .executor()
            .advance_clock(std::time::Duration::from_secs(1));
        test.cx.run_until_parked();

        // After the next restart, what was sent isn't back; the turn is.
        restart(&mut test);
        test.wait("the restore", |turns| {
            turns.pending_file_count(terminal_id) > 0
        });
        test.cx
            .executor()
            .advance_clock(std::time::Duration::from_secs(1));
        test.cx.run_until_parked();
        let review = test.open_review();
        review.read_with(test.cx, |review, cx| {
            assert!(review.unsent_rejected(cx).is_empty());
            assert!(review.carried.is_empty());
            let turns = AgentTurns::try_global(cx).unwrap().read(cx);
            assert_eq!(turns.turns(terminal_id).count(), 1);
        });
    }
}
