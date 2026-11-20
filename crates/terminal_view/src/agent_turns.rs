//! SOLO: what an AI agent in a terminal changed, turn by turn.
//!
//! When an agent starts a turn (its hooks report a prompt, or else its screen
//! shows it at work), its working directory is snapshotted (see
//! [`snapshot`]); when the turn ends, it's snapshotted again and the files
//! that changed are recorded. What the agent changed is pending until the
//! user accepts it: each file is compared with what the user last accepted
//! of it (see [`pending`]). The terminal's tab offers to review the pending
//! changes (see [`review`]), where hunks can be kept or rejected, commented
//! on for the agent, or the work tree rolled back to before any recorded
//! turn. Agents whose status is only guessed from their output have no
//! turns.
//!
//! A directory that can't be snapshotted (not a repository and too big, say)
//! still gets a list of the files that changed during the turn, from the
//! project's file events, without diffs or roll back.

mod diagnostics;
mod pending;
mod review;
mod snapshot;

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use collections::HashMap;
use anyhow::Context as _;
use futures::FutureExt as _;
use futures::future::{BoxFuture, Shared};
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, EntityId, Global, Subscription, Task,
    WeakEntity, Window, actions,
};
use project::{PathChange, UpdatedEntriesSet, Worktree};
use ui::{ContextMenu, PopoverMenu, Tooltip, prelude::*};
use util::ResultExt as _;
use workspace::{Workspace, item::ItemEvent};

use crate::{
    AgentTurnEvent, TerminalView, agent_status::AgentEditedFile, terminal_panel::TerminalPanel,
};
use diagnostics::{DiagnosticsBefore, TurnDiagnostics};
use pending::PendingFile;
pub(crate) use review::AgentTurnReview;
use snapshot::{
    ChangedFile, FileChange, LargeFile, SnapshotStore, SnapshotUnavailable, TreeId,
    changed_large_files,
};

actions!(
    terminal,
    [
        /// Shows what the AI agent in the terminal changed in its last turn,
        /// to keep or reject each change, comment on it, or roll back turns.
        ReviewAgentTurn,
    ]
);

/// Turns remembered per terminal.
const MAX_TURNS: usize = 20;
/// Files noted per turn that can't be snapshotted; more are only counted as
/// "more".
const MAX_WATCHED_FILES: usize = 10_000;
/// How long changes are still noted after a turn without a snapshot ends:
/// the project hears of a file's change a moment after it's written.
const WATCH_AFTER_TURN: Duration = Duration::from_secs(1);
/// Files listed in the menu of a turn without a snapshot.
const MAX_LISTED_FILES: usize = 50;
/// SOLO: How long a Codex turn's end waits for the log events Codex sent
/// last: it sends them every 200 ms in Solo's terminals
/// (`OTEL_BLRP_SCHEDULE_DELAY`), every second by default.
const CODEX_EDITS_WAIT: Duration = Duration::from_millis(1200);
/// SOLO: How long after that a Codex turn's patches that come late still
/// count, and how often they're looked for.
const CODEX_LATE_EDITS: Duration = Duration::from_secs(30);
const CODEX_LATE_EDITS_INTERVAL: Duration = Duration::from_secs(1);
/// SOLO: Slack around a turn's start and end for Codex's patches, whose
/// times are Codex's and the turn's Solo's (on the same clock, but the hooks
/// that start and end the turn take a moment).
const CODEX_EDITS_SLACK_MS: i64 = 2_000;
/// SOLO: How long after a pending file changes on disk its changes are
/// checked again (G59).
const PENDING_REFRESH_DELAY: Duration = Duration::from_millis(500);
/// SOLO: How long after its view goes a terminal's pending changes are
/// forgotten, unless a view of it comes back (it moved) (G59).
const FORGET_CLOSED_TERMINAL_DELAY: Duration = Duration::from_secs(3);

pub(crate) fn init(cx: &mut App) {
    cx.observe_new(|view: &mut TerminalView, _, cx: &mut Context<TerminalView>| {
        // SOLO: its pending changes, kept across restarts and moves; gone
        // with it when it's closed (G59).
        let key = terminal_key(view, cx);
        let project = view.project.clone();
        let terminal_view = cx.entity();
        AgentTurns::global(cx).update(cx, |turns, cx| {
            turns.register_terminal(terminal_view, key, project, cx)
        });
        let terminal_id = cx.entity_id();
        cx.on_release(move |_, cx| {
            if let Some(turns) = AgentTurns::try_global(cx).cloned() {
                // Not as it's being released: from the next effect.
                cx.defer(move |cx| {
                    turns.update(cx, |turns, cx| turns.terminal_released(terminal_id, cx))
                });
            }
        })
        .detach();
        cx.subscribe_self(|view: &mut TerminalView, event: &AgentTurnEvent, cx| {
            let terminal_view = cx.entity();
            match event {
                AgentTurnEvent::Started => {
                    let directory = view.agent_working_directory(cx);
                    let prompt_source = view
                        .agent_status()
                        .is_some_and(|(name, _)| name == "claude")
                        .then(|| directory.clone())
                        .flatten();
                    // What the language servers said before the turn.
                    let project = view.project.clone();
                    let diagnostics_before = project
                        .upgrade()
                        .map(|project| diagnostics::capture_before(&project, cx))
                        .unwrap_or_default();
                    AgentTurns::global(cx).update(cx, |turns, cx| {
                        turns.start_turn(
                            terminal_view,
                            directory,
                            prompt_source,
                            project,
                            diagnostics_before,
                            cx,
                        )
                    });
                    // Here, as the view is being updated already.
                    cx.emit(ItemEvent::UpdateTab);
                }
                AgentTurnEvent::Ended => {
                    // SOLO: whose telemetry says which files its patches edited.
                    let codex_session = view.reported_codex_session();
                    AgentTurns::global(cx).update(cx, |turns, cx| {
                        turns.end_turn(terminal_view, codex_session, cx)
                    });
                }
            }
        })
        .detach();
        // SOLO: the files the agent says it edited (R3 M5).
        cx.subscribe_self(|_: &mut TerminalView, event: &AgentEditedFile, cx| {
            let terminal_id = cx.entity_id();
            let path = event.0.clone();
            AgentTurns::global(cx)
                .update(cx, |turns, cx| turns.file_edited(terminal_id, path, cx));
        })
        .detach();
    })
    .detach();

    cx.observe_new(|workspace: &mut Workspace, _, _| {
        workspace.register_action(review_agent_turn);
    })
    .detach();
}

/// A turn of an agent and what it changed.
#[derive(Clone)]
pub(crate) struct AgentTurn {
    /// Counts the terminal's turns from 1.
    pub number: usize,
    pub started_at: time::OffsetDateTime,
    /// Where the agent worked.
    pub directory: PathBuf,
    /// What the user asked for, when the agent's transcript says.
    pub prompt: Option<String>,
    pub store: Arc<SnapshotStore>,
    pub before: TreeId,
    /// SOLO: The large untracked files the first snapshot left out (G51).
    pub before_large: Arc<Vec<LargeFile>>,
    /// The work tree when the turn ended, to tell the agent's changes from
    /// ones made since.
    pub after: TreeId,
    pub changed: Vec<ChangedFile>,
    /// SOLO: Whether `changed` are the files the agent's hooks said it
    /// edited. If not (it doesn't report them), they're everything that
    /// changed in the work tree during the turn, some maybe by the user or
    /// another agent (R3 M5).
    pub attributed: bool,
    /// SOLO: When `attributed`, the other files that changed in the work
    /// tree during the turn: by the agent's commands, the user or another
    /// agent. Rejecting the turn leaves them alone.
    pub others: Vec<ChangedFile>,
    /// SOLO: Files the agent edited before the turn's first snapshot was
    /// taken: what it changed in them may show only in part.
    pub edited_before_snapshot: Vec<PathBuf>,
    /// What the turn did to the language servers' diagnostics.
    pub diagnostics: TurnDiagnostics,
    /// SOLO: Large untracked files that changed during the turn, which no
    /// snapshot has: the review can't show them, nor a rollback restore
    /// them (G51).
    pub large_files: Vec<String>,
}

/// A finished turn whose directory couldn't be snapshotted: only which files
/// the project saw change during it.
pub(crate) struct UnsnapshottedTurn {
    pub number: usize,
    pub directory: PathBuf,
    /// Why there's no snapshot, so no diff or roll back.
    pub reason: SharedString,
    /// Absolute paths, sorted.
    pub changed: Vec<(PathBuf, FileChange)>,
    /// Whether more files changed than were noted.
    pub more: bool,
}

struct StartedTurn {
    store: Arc<SnapshotStore>,
    before: TreeId,
    before_large: Arc<Vec<LargeFile>>,
}

/// A turn's snapshot, or why there's none.
type TurnSnapshot = Result<Arc<StartedTurn>, SharedString>;

struct PendingTurn {
    number: usize,
    started_at: time::OffsetDateTime,
    directory: PathBuf,
    prompt_source: Option<PathBuf>,
    project: WeakEntity<project::Project>,
    diagnostics_before: DiagnosticsBefore,
    snapshot: Shared<BoxFuture<'static, TurnSnapshot>>,
    /// SOLO: The files the agent's hooks said it edited during the turn.
    edited: Vec<PathBuf>,
    /// SOLO: Those it said it edited before the first snapshot was taken.
    edited_before_snapshot: Vec<PathBuf>,
}

/// The files in a turn's directory the project saw change since the turn
/// started, kept for when there's no snapshot.
struct TurnWatch {
    number: usize,
    directory: PathBuf,
    changes: WatchedChanges,
    _subscription: Subscription,
}

#[derive(Default)]
struct WatchedChanges {
    /// By absolute path. Removed directories are here too until the turn's
    /// end, as their entries are gone and they can't be told from files.
    files: BTreeMap<PathBuf, FileChange>,
    more: bool,
}

#[derive(Default)]
struct TerminalTurns {
    turns_started: usize,
    /// Finished turns that changed files, oldest first.
    turns: VecDeque<AgentTurn>,
    /// The last finished turn that couldn't be snapshotted.
    unsnapshotted: Option<Arc<UnsnapshottedTurn>>,
    pending: Option<PendingTurn>,
    /// Turns whose changes are being noted.
    watches: Vec<TurnWatch>,
    /// Whether the last finished turn changed files, so its tab offers them.
    offer_last_turn: bool,
    /// Stores by directory, which keep their index between snapshots.
    stores: HashMap<PathBuf, Arc<SnapshotStore>>,
    /// SOLO: Whether the agent reports the files it edits, so its turns
    /// are only those (R3 M5).
    reports_edits: bool,
    _end_task: Option<Task<()>>,
    /// SOLO: What the agent changed that the user hasn't accepted, in each
    /// folder it worked in (G59).
    changes: Vec<PendingChanges>,
    /// SOLO: The turns whose ends are being recorded, one after another:
    /// the files they changed stay locked until their changes are pending.
    ending: Vec<ActiveTurn>,
    /// SOLO: The terminal's id (`SOLO_TERMINAL_ID`), which its pending
    /// changes are recorded by.
    key: Option<String>,
    project: Option<WeakEntity<project::Project>>,
    /// SOLO: How many of the terminal's review pages are open: earlier
    /// turns' changes aren't accepted then.
    reviews_open: usize,
    /// SOLO: Whether earlier turns' changes are to be accepted once the
    /// review closes.
    auto_keep_due: bool,
    save_task: Option<Task<()>>,
    refresh_task: Option<Task<()>>,
    _watch_files: Option<Subscription>,
    view: Option<WeakEntity<TerminalView>>,
}

/// SOLO: A terminal's pending changes: for each file its agent changed,
/// what the user last accepted of it (G59).
#[derive(Clone)]
pub(crate) struct PendingChanges {
    pub store: Arc<SnapshotStore>,
    /// Where the agent works.
    pub directory: PathBuf,
    pub files: BTreeMap<String, PendingFile>,
    /// The files that differ from what's accepted, as last checked.
    pub pending: BTreeSet<String>,
    /// The trees the record keeps.
    trees: Vec<TreeId>,
    /// Counts the changes, for views to reload.
    pub revision: usize,
    /// When the agent last worked in its folder, by the terminal's count:
    /// the latest is the one the review shows.
    used: usize,
}

/// SOLO: A turn under way or ending, whose files are locked (G59).
#[derive(Clone)]
pub(crate) struct ActiveTurn {
    pub number: usize,
    /// The turn's first snapshot, once it's taken.
    pub before: Option<(Arc<SnapshotStore>, TreeId)>,
    /// The files the agent reported editing, if it does.
    pub edited: Option<Vec<PathBuf>>,
}

impl TerminalTurns {
    fn finished_turns(&self) -> usize {
        self.turns_started
            .saturating_sub(usize::from(self.pending.is_some()))
    }

    /// While the last finished turn's changes are offered on the tab.
    fn offering(&self) -> bool {
        self.offer_last_turn && self.pending.is_none()
    }

    /// SOLO: The pending changes in the folder the agent worked in last.
    fn current_changes(&self) -> Option<&PendingChanges> {
        self.changes.iter().max_by_key(|changes| changes.used)
    }

    fn changes_in(&self, store_dir: &Path) -> Option<&PendingChanges> {
        self.changes
            .iter()
            .find(|changes| changes.store.store_dir() == store_dir)
    }

    fn changes_in_mut(&mut self, store_dir: &Path) -> Option<&mut PendingChanges> {
        self.changes
            .iter_mut()
            .find(|changes| changes.store.store_dir() == store_dir)
    }

    fn next_use(&self) -> usize {
        self.changes.iter().map(|changes| changes.used).max().unwrap_or(0) + 1
    }
}

#[derive(Default)]
pub(crate) struct AgentTurns {
    terminals: HashMap<EntityId, TerminalTurns>,
    /// Where snapshots are stored, if not in Solo's data directory.
    storage_root: Option<PathBuf>,
    /// How many files a directory outside a repository may have to be
    /// snapshotted, if not the usual.
    #[cfg(test)]
    max_files_outside_repository: Option<usize>,
    /// How big an untracked file may be to be snapshotted, if not the usual.
    #[cfg(test)]
    max_untracked_file_bytes: Option<u64>,
    /// SOLO: Whether Solo is quitting: its terminals' views go, but not the
    /// terminals (G59).
    quitting: bool,
    /// SOLO: The ids of the terminals with views, to tell one that was
    /// closed from one that moved.
    keys: HashMap<EntityId, String>,
}

struct GlobalAgentTurns(Entity<AgentTurns>);

impl Global for GlobalAgentTurns {}

impl AgentTurns {
    pub(crate) fn global(cx: &mut App) -> Entity<Self> {
        if let Some(global) = cx.try_global::<GlobalAgentTurns>() {
            return global.0.clone();
        }
        let turns = cx.new(|cx| {
            cx.on_app_quit(|this: &mut AgentTurns, _| {
                this.quitting = true;
                async {}
            })
            .detach();
            AgentTurns::default()
        });
        cx.set_global(GlobalAgentTurns(turns.clone()));
        turns
    }

    pub(crate) fn try_global(cx: &App) -> Option<&Entity<Self>> {
        cx.try_global::<GlobalAgentTurns>().map(|global| &global.0)
    }

    /// The recorded turns of a terminal, oldest first.
    pub(crate) fn turns(&self, terminal: EntityId) -> impl DoubleEndedIterator<Item = &AgentTurn> {
        self.terminals
            .get(&terminal)
            .into_iter()
            .flat_map(|turns| turns.turns.iter())
    }

    /// Whether the snapshot starting the terminal's current turn is taken
    /// (it's taken by git on a background thread).
    #[cfg(test)]
    pub(crate) fn start_snapshot_taken(&self, terminal: EntityId) -> bool {
        self.terminals
            .get(&terminal)
            .and_then(|turns| turns.pending.as_ref())
            .is_some_and(|pending| pending.snapshot.peek().is_some())
    }

    pub(crate) fn turn(&self, terminal: EntityId, number: usize) -> Option<&AgentTurn> {
        self.turns(terminal).find(|turn| turn.number == number)
    }

    /// The terminal whose agent last changed the file at `abs_path`.
    pub(crate) fn last_to_change(&self, abs_path: &std::path::Path) -> Option<EntityId> {
        self.terminals
            .iter()
            .flat_map(|(terminal, turns)| turns.turns.iter().map(move |turn| (*terminal, turn)))
            .filter(|(_, turn)| {
                turn.changed
                    .iter()
                    .any(|file| turn.store.abs_path(&file.path) == abs_path)
            })
            .max_by_key(|(_, turn)| turn.started_at)
            .map(|(terminal, _)| terminal)
    }

    /// How many files the terminal's last finished turn changed; `None`
    /// before a turn has finished.
    pub(crate) fn last_turn_changed_files(&self, terminal: EntityId) -> Option<usize> {
        let turns = self.terminals.get(&terminal)?;
        let finished = turns.finished_turns();
        if finished == 0 {
            return None;
        }
        let snapshotted = turns
            .turns
            .back()
            .filter(|turn| turn.number == finished)
            .map(|turn| turn.changed.len());
        let unsnapshotted = turns
            .unsnapshotted
            .as_ref()
            .filter(|turn| turn.number == finished)
            .map(|turn| turn.changed.len());
        Some(snapshotted.or(unsnapshotted).unwrap_or(0))
    }

    /// The files the last turn changed, while its tab offers them.
    #[cfg(test)]
    fn offered_turn(&self, terminal: EntityId) -> Option<&AgentTurn> {
        let turns = self.terminals.get(&terminal)?;
        if !turns.offering() {
            return None;
        }
        let finished = turns.finished_turns();
        turns.turns.back().filter(|turn| turn.number == finished)
    }

    /// The files the last turn changed when it couldn't be snapshotted,
    /// while its tab offers them.
    fn offered_unsnapshotted_turn(&self, terminal: EntityId) -> Option<&Arc<UnsnapshottedTurn>> {
        let turns = self.terminals.get(&terminal)?;
        if !turns.offering() {
            return None;
        }
        let finished = turns.finished_turns();
        turns
            .unsnapshotted
            .as_ref()
            .filter(|turn| turn.number == finished)
    }

    fn start_turn(
        &mut self,
        terminal_view: Entity<TerminalView>,
        directory: Option<PathBuf>,
        prompt_source: Option<PathBuf>,
        project: WeakEntity<project::Project>,
        diagnostics_before: DiagnosticsBefore,
        cx: &mut Context<Self>,
    ) {
        let Some(directory) = directory else {
            log::debug!("no working directory to snapshot for an agent turn");
            return;
        };
        let terminal_id = terminal_view.entity_id();
        #[cfg(test)]
        let max_files_outside_repository = self.max_files_outside_repository;
        #[cfg(test)]
        let max_untracked_file_bytes = self.max_untracked_file_bytes;
        let storage_root = self
            .storage_root
            .clone()
            .unwrap_or_else(|| paths::data_dir().join("agent_turns"));
        let terminal = self.terminals.entry(terminal_id).or_default();
        if terminal.view.is_none() {
            terminal.view = Some(terminal_view.downgrade());
        }
        if terminal.project.is_none() {
            terminal.project = Some(project.clone());
        }
        if let Some(previous) = terminal.pending.take() {
            terminal
                .watches
                .retain(|watch| watch.number != previous.number);
        }
        terminal.turns_started += 1;
        terminal.offer_last_turn = false;
        let number = terminal.turns_started;
        let known_store = terminal.stores.get(&directory).cloned();
        let snapshot = cx
            .background_spawn({
                let directory = directory.clone();
                async move {
                    let store = match known_store {
                        Some(store) => store,
                        None => {
                            #[allow(unused_mut)]
                            let mut store = SnapshotStore::open(&directory, &storage_root)?;
                            #[cfg(test)]
                            if let Some(max) = max_files_outside_repository {
                                store.set_max_files_outside_repository(max);
                            }
                            #[cfg(test)]
                            if let Some(max) = max_untracked_file_bytes {
                                store.set_max_untracked_file_bytes(max);
                            }
                            Arc::new(store)
                        }
                    };
                    let started = std::time::Instant::now();
                    let (before, before_large) = store.snapshot_with_large_files()?;
                    log::debug!(
                        "snapshotted {:?} for an agent turn in {:?}",
                        store.work_tree(),
                        started.elapsed()
                    );
                    anyhow::Ok(Arc::new(StartedTurn {
                        store,
                        before,
                        before_large: Arc::new(before_large),
                    }))
                }
            })
            .map(|result| result.map_err(|error| snapshot_unavailable_reason(&error)))
            .boxed()
            .shared();
        // What changed, in case there's no snapshot.
        let subscription = project.upgrade().map(|project| {
            cx.subscribe(&project, move |this, project, event, cx| {
                let project::Event::WorktreeUpdatedEntries(worktree_id, changes) = event else {
                    return;
                };
                let Some(watch) = this.terminals.get_mut(&terminal_id).and_then(|terminal| {
                    terminal
                        .watches
                        .iter_mut()
                        .find(|watch| watch.number == number)
                }) else {
                    return;
                };
                if let Some(worktree) = project.read(cx).worktree_for_id(*worktree_id, cx) {
                    watch_changes(
                        &mut watch.changes,
                        &watch.directory,
                        worktree.read(cx),
                        changes,
                    );
                }
            })
        });
        if let Some(subscription) = subscription {
            terminal.watches.push(TurnWatch {
                number,
                directory: directory.clone(),
                changes: WatchedChanges::default(),
                _subscription: subscription,
            });
        }
        terminal.pending = Some(PendingTurn {
            number,
            started_at: now(),
            directory: directory.clone(),
            prompt_source,
            project,
            diagnostics_before,
            snapshot: snapshot.clone(),
            edited: Vec::new(),
            edited_before_snapshot: Vec::new(),
        });
        // SOLO: reviews lock the files it changes (G59).
        cx.notify();
        // Keep the store for the next turn once it's open.
        cx.spawn(async move |this, cx| {
            if let Ok(started) = snapshot.await {
                this.update(cx, |this, cx| {
                    if let Some(terminal) = this.terminals.get_mut(&terminal_id) {
                        terminal.stores.insert(directory, started.store.clone());
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// SOLO: The terminal's agent says it edited `path` (R3 M5).
    fn file_edited(&mut self, terminal: EntityId, path: PathBuf, cx: &mut Context<Self>) {
        // SOLO: reviews lock the file (G59).
        cx.notify();
        let terminal = self.terminals.entry(terminal).or_default();
        terminal.reports_edits = true;
        let Some(pending) = terminal.pending.as_mut() else {
            log::debug!("the agent reported editing {path:?} outside a turn");
            return;
        };
        log::debug!("the agent reported editing {path:?}");
        if pending.snapshot.peek().is_none() && !pending.edited_before_snapshot.contains(&path) {
            pending.edited_before_snapshot.push(path.clone());
        }
        if !pending.edited.contains(&path) {
            pending.edited.push(path);
        }
    }

    /// Ends the terminal's turn. `codex_session` is the Codex session the
    /// turn was, whose log events say which files its patches edited.
    fn end_turn(
        &mut self,
        terminal_view: Entity<TerminalView>,
        codex_session: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let terminal_id = terminal_view.entity_id();
        let Some(terminal) = self.terminals.get_mut(&terminal_id) else {
            return;
        };
        let Some(mut pending) = terminal.pending.take() else {
            return;
        };
        let reports_edits = terminal.reports_edits;
        // SOLO: its files stay locked until its changes are pending (G59).
        terminal.ending.push(ActiveTurn {
            number: pending.number,
            before: match pending.snapshot.peek() {
                Some(Ok(started)) => Some((started.store.clone(), started.before.clone())),
                _ => None,
            },
            edited: reports_edits.then(|| pending.edited.clone()),
        });
        let number = pending.number;
        // SOLO: the turn's time, for Codex's patches (R3 M5).
        let window = CodexWindow {
            from_ms: unix_ms(pending.started_at) - CODEX_EDITS_SLACK_MS,
            to_ms: unix_ms(now()) + CODEX_EDITS_SLACK_MS,
        };
        let codex_session =
            codex_session.filter(|_| agent_sessions::codex_otel::running_receiver().is_some());
        let terminal_view = terminal_view.downgrade();
        // SOLO: after the earlier turn's end is recorded, not in place of
        // it: its changes would skip the review (G59).
        let earlier = terminal._end_task.take();
        terminal._end_task = Some(cx.spawn(async move |this, cx| {
            if let Some(earlier) = earlier {
                earlier.await;
            }
            let started = match pending.snapshot.await {
                Ok(started) => started,
                Err(reason) => {
                    // The last changes still on their way.
                    cx.background_executor().timer(WATCH_AFTER_TURN).await;
                    this.update(cx, |this, cx| {
                        this.end_unsnapshotted_turn(
                            terminal_id,
                            pending.number,
                            pending.directory,
                            reason,
                            cx,
                        );
                        this.turn_ended(terminal_id, number, cx);
                        update_tab(&terminal_view, cx);
                    })
                    .ok();
                    return;
                }
            };
            this.update(cx, |this, _| {
                this.stop_watching(terminal_id, pending.number)
            })
            .ok();
            let prompt_source = pending.prompt_source;
            let started_at = pending.started_at;
            let project = pending.project;
            let diagnostics_before = pending.diagnostics_before;
            let mut edited = std::mem::take(&mut pending.edited);
            let directory = pending.directory.clone();
            let after = cx
                .background_spawn({
                    let started = started.clone();
                    async move {
                        let (after, after_large) = started.store.snapshot_with_large_files()?;
                        let changed = started.store.changed_files(&started.before, &after)?;
                        let large_files = changed_large_files(&started.before_large, &after_large);
                        anyhow::Ok((after, changed, large_files))
                    }
                })
                .await
                .log_err();
            let Some((after, changed, large_files)) = after else {
                this.update(cx, |this, cx| this.turn_ended(terminal_id, number, cx))
                    .ok();
                return;
            };
            // SOLO: the files Codex's patches edited, from its log events,
            // which come in batches: the last ones a moment after the turn.
            let mut codex_attributed = false;
            if let Some(session) = &codex_session {
                cx.background_executor().timer(CODEX_EDITS_WAIT).await;
                if let Some(codex_edited) = codex_edits(session, window, &directory) {
                    codex_attributed = true;
                    for path in codex_edited {
                        if !edited.contains(&path) {
                            edited.push(path);
                        }
                    }
                }
            }
            let attributed = reports_edits || codex_attributed;
            let changes = cx
                .background_spawn({
                    let started = started.clone();
                    async move {
                        // SOLO: an agent that reports its edits changed only
                        // those files; the rest are someone else's, or its
                        // commands' (R3 M5).
                        let (changed, others) = if attributed {
                            let edited: Vec<PathBuf> =
                                edited.iter().map(|path| comparable_path(path)).collect();
                            let (changed, others): (Vec<_>, Vec<_>) =
                                changed.into_iter().partition(|file| {
                                    let abs_path =
                                        comparable_path(&started.store.abs_path(&file.path));
                                    edited.iter().any(|edited| is_same_path(edited, &abs_path))
                                });
                            log::debug!(
                                "the turn's reported edits {edited:?}: {} of its changed files are the agent's, {} others",
                                changed.len(),
                                others.len()
                            );
                            (changed, others)
                        } else {
                            (changed, Vec::new())
                        };
                        let prompt = prompt_source
                            .and_then(|directory| last_claude_prompt(&directory, started_at));
                        anyhow::Ok((changed, others, prompt))
                    }
                })
                .await
                .log_err();
            let Some((changed, others, prompt)) = changes else {
                this.update(cx, |this, cx| this.turn_ended(terminal_id, number, cx))
                    .ok();
                return;
            };
            // SOLO: what the turn changed is pending now, and what earlier
            // turns did is accepted (G59).
            let turn_end = this
                .update(cx, |this, _| {
                    this.turn_end_input(terminal_id, &started, &after, number, started_at, &changed)
                })
                .ok()
                .flatten();
            // The files to have the language servers check: those still there.
            let changed_paths: Vec<PathBuf> = changed
                .iter()
                .filter(|file| file.change != FileChange::Deleted)
                .map(|file| started.store.abs_path(&file.path))
                .collect();
            let recorded = this.update(cx, |this, cx| {
                let Some(terminal) = this.terminals.get_mut(&terminal_id) else {
                    return false;
                };
                terminal.offer_last_turn = !changed.is_empty();
                let recorded =
                    !changed.is_empty() || !others.is_empty() || !large_files.is_empty();
                let mut evicted = false;
                if recorded {
                    terminal.turns.push_back(AgentTurn {
                        number: pending.number,
                        started_at,
                        directory: pending.directory,
                        prompt,
                        store: started.store.clone(),
                        before: started.before.clone(),
                        before_large: started.before_large.clone(),
                        after,
                        changed,
                        attributed,
                        others,
                        edited_before_snapshot: pending.edited_before_snapshot,
                        diagnostics: TurnDiagnostics::Checking,
                        large_files,
                    });
                    if terminal.turns.len() > MAX_TURNS {
                        terminal.turns.pop_front();
                        evicted = true;
                    }
                }
                // SOLO: the objects only the forgotten turn needed go (G51).
                if evicted {
                    this.collect_garbage(&started.store, cx);
                }
                cx.notify();
                update_tab(&terminal_view, cx);
                recorded
            });
            // SOLO: what the turn changed is pending now, and what only
            // earlier turns did is accepted (G59).
            if let Some((key, files, turn_end)) = turn_end {
                let result = cx
                    .background_spawn(async move {
                        let files = pending::end_turn(&files, &turn_end)?;
                        anyhow::Ok((files, turn_end.store))
                    })
                    .await;
                this.update(cx, |this, cx| {
                    match result {
                        Ok((files, store)) => {
                            this.set_pending_files(terminal_id, key, store, &directory, files, cx);
                        }
                        Err(error) => log::error!("failed to record pending changes: {error:#}"),
                    }
                    update_tab(&terminal_view, cx);
                })
                .ok();
            }
            this.update(cx, |this, cx| this.turn_ended(terminal_id, number, cx))
                .ok();
            if !matches!(recorded, Ok(true)) {
                return;
            }
            // SOLO: Codex's patches whose events come later still count.
            if codex_attributed && let Some(session) = codex_session {
                let terminal_view = terminal_view.clone();
                let directory = directory.clone();
                let this = this.clone();
                cx.spawn(async move |cx| {
                    let started = std::time::Instant::now();
                    while started.elapsed() < CODEX_LATE_EDITS {
                        cx.background_executor()
                            .timer(CODEX_LATE_EDITS_INTERVAL)
                            .await;
                        let Some(edited) = codex_edits(&session, window, &directory) else {
                            continue;
                        };
                        let moved = this.update(cx, |this, cx| {
                            let moved = this.late_edits(terminal_id, number, &edited);
                            if moved {
                                cx.notify();
                                update_tab(&terminal_view, cx);
                            }
                        });
                        if moved.is_err() {
                            return;
                        }
                    }
                })
                .detach();
            }
            // Checked apart: the next turn's end mustn't cancel it.
            cx.spawn(async move |cx| {
                let result =
                    diagnostics::check_turn(project, diagnostics_before, changed_paths, cx).await;
                this.update(cx, |this, cx| {
                    let turn = this.terminals.get_mut(&terminal_id).and_then(|terminal| {
                        terminal.turns.iter_mut().find(|turn| turn.number == number)
                    });
                    if let Some(turn) = turn {
                        turn.diagnostics = result;
                        cx.notify();
                        update_tab(&terminal_view, cx);
                    }
                })
                .ok();
            })
            .detach();
        }));
    }

    /// SOLO: Codex's patches of turn `number` that came after it ended:
    /// the files they edited are the turn's rather than someone else's.
    /// Whether any moved.
    fn late_edits(&mut self, terminal_id: EntityId, number: usize, edited: &[PathBuf]) -> bool {
        let Some(terminal) = self.terminals.get_mut(&terminal_id) else {
            return false;
        };
        let is_last = terminal
            .turns
            .back()
            .is_some_and(|turn| turn.number == number);
        let Some(turn) = terminal.turns.iter_mut().find(|turn| turn.number == number) else {
            return false;
        };
        if turn.others.is_empty() || edited.is_empty() {
            return false;
        }
        let edited: Vec<PathBuf> = edited.iter().map(|path| comparable_path(path)).collect();
        let store = turn.store.clone();
        let (late, others): (Vec<_>, Vec<_>) = std::mem::take(&mut turn.others)
            .into_iter()
            .partition(|file| {
                let abs_path = comparable_path(&store.abs_path(&file.path));
                edited.iter().any(|edited| is_same_path(edited, &abs_path))
            });
        turn.others = others;
        if late.is_empty() {
            return false;
        }
        log::debug!(
            "{} of a Codex turn's files turned out to be its own",
            late.len()
        );
        turn.changed.extend(late);
        turn.changed.sort_by(|a, b| a.path.cmp(&b.path));
        if is_last && terminal.pending.is_none() {
            terminal.offer_last_turn = true;
        }
        true
    }

    /// SOLO: The trees of `store_dir`'s store that turns of any terminal
    /// still need (G51).
    fn live_trees(&self, store_dir: &Path) -> Vec<TreeId> {
        let mut trees = Vec::new();
        for terminal in self.terminals.values() {
            for turn in &terminal.turns {
                if turn.store.store_dir() == store_dir {
                    trees.push(turn.before.clone());
                    trees.push(turn.after.clone());
                }
            }
            if let Some(Ok(started)) = terminal
                .pending
                .as_ref()
                .and_then(|pending| pending.snapshot.peek())
                && started.store.store_dir() == store_dir
            {
                trees.push(started.before.clone());
            }
            // SOLO: and what the user accepted of files with pending
            // changes (G59).
            for changes in &terminal.changes {
                if changes.store.store_dir() == store_dir {
                    trees.extend(changes.trees.iter().cloned());
                }
            }
        }
        trees
    }

    /// SOLO: Removes the objects of `store` that no turn needs, in the
    /// background (G51).
    fn collect_garbage(&self, store: &Arc<SnapshotStore>, cx: &mut Context<Self>) {
        let live = self.live_trees(store.store_dir());
        let store = store.clone();
        cx.background_spawn(async move {
            if let Err(error) = store.collect_garbage(live) {
                log::warn!(
                    "failed to collect the snapshots of {:?}: {error:#}",
                    store.work_tree()
                );
            }
        })
        .detach();
    }

    fn stop_watching(&mut self, terminal: EntityId, number: usize) -> Option<TurnWatch> {
        let watches = &mut self.terminals.get_mut(&terminal)?.watches;
        let ix = watches.iter().position(|watch| watch.number == number)?;
        Some(watches.remove(ix))
    }

    /// Records a turn that couldn't be snapshotted with the files the project
    /// saw change.
    fn end_unsnapshotted_turn(
        &mut self,
        terminal_id: EntityId,
        number: usize,
        directory: PathBuf,
        reason: SharedString,
        cx: &mut Context<Self>,
    ) {
        let changes = self
            .stop_watching(terminal_id, number)
            .map(|watch| watch.changes)
            .unwrap_or_default();
        let Some(terminal) = self.terminals.get_mut(&terminal_id) else {
            return;
        };
        let changed = changed_files(changes.files);
        terminal.offer_last_turn = !changed.is_empty();
        terminal.unsnapshotted = Some(Arc::new(UnsnapshottedTurn {
            number,
            directory,
            reason,
            changed,
            more: changes.more,
        }));
        cx.notify();
    }

    /// Stops offering the last turn's changes on the terminal's tab.
    pub(crate) fn dismiss_offer(&mut self, terminal: EntityId, cx: &mut Context<Self>) {
        if let Some(turns) = self.terminals.get_mut(&terminal) {
            turns.offer_last_turn = false;
            cx.notify();
        }
    }

    fn storage_root(&self) -> PathBuf {
        self.storage_root
            .clone()
            .unwrap_or_else(|| paths::data_dir().join("agent_turns"))
    }

    /// SOLO: Notes a terminal's view: the terminal's id, which its pending
    /// changes are recorded by, and those changes if they were recorded
    /// before (Solo restarted, or the terminal moved) (G59).
    fn register_terminal(
        &mut self,
        view: Entity<TerminalView>,
        key: Option<String>,
        project: WeakEntity<project::Project>,
        cx: &mut Context<Self>,
    ) {
        let terminal_id = view.entity_id();
        let terminal = self.terminals.entry(terminal_id).or_default();
        terminal.project = Some(project);
        terminal.view = Some(view.downgrade());
        let Some(key) = key else {
            return;
        };
        terminal.key = Some(key.clone());
        self.keys.insert(terminal_id, key.clone());
        let storage_root = self.storage_root();
        let view = view.downgrade();
        cx.spawn(async move |this, cx| {
            let restored = cx
                .background_spawn(async move { restore_pending(&storage_root, &key) })
                .await;
            if restored.is_empty() {
                return;
            }
            this.update(cx, |this, cx| {
                let Some(terminal) = this.terminals.get_mut(&terminal_id) else {
                    return;
                };
                for (store, directory, files, modified) in restored {
                    // A turn there ended meanwhile: its changes are newer.
                    if terminal.changes_in(store.store_dir()).is_some() {
                        continue;
                    }
                    log::info!(
                        "restored the pending changes of {} files in {directory:?}",
                        files.len()
                    );
                    terminal.stores.insert(directory.clone(), store.clone());
                    terminal.changes.push(PendingChanges {
                        store,
                        directory,
                        pending: files.keys().cloned().collect(),
                        files,
                        trees: Vec::new(),
                        revision: 1,
                        used: modified,
                    });
                }
                this.save_pending(terminal_id, cx);
                this.watch_pending_files(terminal_id, cx);
                this.refresh_pending(terminal_id, cx);
                update_tab(&view, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// SOLO: A terminal's view went. Unless Solo is quitting (the terminal
    /// outlives it) or a view of the terminal comes back soon (it moved),
    /// its pending changes go with its turns (G59).
    fn terminal_released(&mut self, terminal_id: EntityId, cx: &mut Context<Self>) {
        let key = self.keys.remove(&terminal_id);
        if self.quitting {
            return;
        }
        let terminal = self.terminals.remove(&terminal_id);
        cx.notify();
        let (Some(key), Some(terminal)) = (key, terminal) else {
            return;
        };
        let stores: Vec<Arc<SnapshotStore>> = terminal
            .changes
            .into_iter()
            .map(|changes| changes.store)
            .collect();
        if stores.is_empty() {
            return;
        }
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(FORGET_CLOSED_TERMINAL_DELAY)
                .await;
            let moved = this
                .read_with(cx, |this, _| this.keys.values().any(|other| *other == key))
                .unwrap_or(true);
            if moved {
                return;
            }
            cx.background_spawn(async move {
                for store in stores {
                    match store.lock() {
                        Ok(lock) => lock.remove_record(&key),
                        Err(error) => log::warn!("failed to forget pending changes: {error:#}"),
                    }
                }
            })
            .await;
        })
        .detach();
    }

    /// SOLO: What the terminal's agent changed that the user hasn't
    /// accepted, in the folder it worked in last (G59).
    pub(crate) fn pending_changes(&self, terminal: EntityId) -> Option<&PendingChanges> {
        self.terminals.get(&terminal)?.current_changes()
    }

    /// SOLO: The terminal's pending changes in the store at `store_dir`.
    pub(crate) fn pending_changes_in(
        &self,
        terminal: EntityId,
        store_dir: &Path,
    ) -> Option<&PendingChanges> {
        self.terminals.get(&terminal)?.changes_in(store_dir)
    }

    /// SOLO: How many files have changes to review, in all the folders the
    /// terminal's agent worked in.
    pub(crate) fn pending_file_count(&self, terminal: EntityId) -> usize {
        self.terminals.get(&terminal).map_or(0, |terminal| {
            terminal
                .changes
                .iter()
                .map(|changes| changes.pending.len())
                .sum()
        })
    }

    /// SOLO: The terminal's turns under way or ending, whose files are
    /// locked.
    pub(crate) fn active_turns(&self, terminal: EntityId) -> Vec<ActiveTurn> {
        let Some(turns) = self.terminals.get(&terminal) else {
            return Vec::new();
        };
        let running = turns.pending.as_ref().map(|pending| ActiveTurn {
            number: pending.number,
            before: match pending.snapshot.peek() {
                Some(Ok(started)) => Some((started.store.clone(), started.before.clone())),
                _ => None,
            },
            edited: turns.reports_edits.then(|| pending.edited.clone()),
        });
        running
            .into_iter()
            .chain(turns.ending.iter().cloned())
            .collect()
    }

    /// SOLO: The input to update the pending changes at the end of turn
    /// `number`: those in the turn's folder. Other folders' are left as
    /// they are.
    fn turn_end_input(
        &mut self,
        terminal_id: EntityId,
        started: &StartedTurn,
        after: &TreeId,
        number: usize,
        started_at: time::OffsetDateTime,
        changed: &[ChangedFile],
    ) -> Option<(
        Option<String>,
        BTreeMap<String, PendingFile>,
        pending::TurnEnd,
    )> {
        let terminal = self.terminals.get_mut(&terminal_id)?;
        let auto_keep = terminal.reviews_open == 0;
        let existing = terminal.changes_in(started.store.store_dir());
        let files = existing
            .map(|changes| changes.files.clone())
            .unwrap_or_default();
        if files.is_empty() && changed.is_empty() && existing.is_none() {
            return None;
        }
        if !auto_keep && !files.is_empty() {
            terminal.auto_keep_due = true;
        }
        Some((
            terminal.key.clone(),
            files,
            pending::TurnEnd {
                store: started.store.clone(),
                before: started.before.clone(),
                after: after.clone(),
                number,
                started_at: SystemTime::from(started_at),
                changed: changed.to_vec(),
                auto_keep,
            },
        ))
    }

    /// SOLO: Turn `number`'s end is recorded: its files aren't locked
    /// anymore.
    fn turn_ended(&mut self, terminal_id: EntityId, number: usize, cx: &mut Context<Self>) {
        if let Some(terminal) = self.terminals.get_mut(&terminal_id) {
            let before = terminal.ending.len();
            terminal.ending.retain(|ending| ending.number != number);
            if terminal.ending.len() != before {
                cx.notify();
            }
        }
    }

    /// SOLO: The terminal's pending changes in `store` are now `files`:
    /// recorded, and views reload them. It's the folder its agent worked in
    /// last.
    fn set_pending_files(
        &mut self,
        terminal_id: EntityId,
        key: Option<String>,
        store: Arc<SnapshotStore>,
        directory: &Path,
        files: BTreeMap<String, PendingFile>,
        cx: &mut Context<Self>,
    ) {
        let Some(terminal) = self.terminals.get_mut(&terminal_id) else {
            return;
        };
        if terminal.key.is_none() {
            terminal.key = key;
        }
        let used = terminal.next_use();
        let pending = files.keys().cloned().collect();
        match terminal.changes_in_mut(store.store_dir()) {
            Some(changes) => {
                changes.directory = directory.to_path_buf();
                changes.files = files;
                changes.pending = pending;
                changes.revision += 1;
                changes.used = used;
            }
            None => terminal.changes.push(PendingChanges {
                store,
                directory: directory.to_path_buf(),
                files,
                pending,
                trees: Vec::new(),
                revision: 1,
                used,
            }),
        }
        self.save_pending(terminal_id, cx);
        self.watch_pending_files(terminal_id, cx);
        cx.notify();
    }

    /// SOLO: The review changed some of the terminal's pending files in the
    /// store at `store_dir`: each `(path, file, whether it still has
    /// changes to review)`; `None` forgets one (G59). Views don't reload
    /// for it.
    pub(crate) fn update_pending_files(
        &mut self,
        terminal_id: EntityId,
        store_dir: &Path,
        updates: Vec<(String, Option<PendingFile>, bool)>,
        cx: &mut Context<Self>,
    ) {
        let Some(changes) = self
            .terminals
            .get_mut(&terminal_id)
            .and_then(|terminal| terminal.changes_in_mut(store_dir))
        else {
            return;
        };
        for (path, file, pending) in updates {
            match file {
                Some(file) if !file.is_accepted() || pending => {
                    changes.files.insert(path.clone(), file);
                }
                _ => {
                    changes.files.remove(&path);
                }
            }
            if pending {
                changes.pending.insert(path);
            } else {
                changes.pending.remove(&path);
            }
        }
        self.save_pending(terminal_id, cx);
        if let Some(view) = self.terminals.get(&terminal_id).and_then(|t| t.view.clone()) {
            update_tab(&view, cx);
        }
        cx.notify();
    }

    /// SOLO: Puts the terminal's pending files in the store at `store_dir`
    /// back as they were (undoing a rollback, say); views reload them.
    pub(crate) fn replace_pending_files(
        &mut self,
        terminal_id: EntityId,
        store_dir: &Path,
        files: BTreeMap<String, PendingFile>,
        cx: &mut Context<Self>,
    ) {
        let Some(changes) = self
            .terminals
            .get_mut(&terminal_id)
            .and_then(|terminal| terminal.changes_in_mut(store_dir))
        else {
            return;
        };
        changes.pending = files.keys().cloned().collect();
        changes.files = files;
        changes.revision += 1;
        self.save_pending(terminal_id, cx);
        self.refresh_pending(terminal_id, cx);
        cx.notify();
    }

    /// SOLO: Records the terminal's pending changes, a moment from now: the
    /// latest of them, if more come meanwhile.
    fn save_pending(&mut self, terminal_id: EntityId, cx: &mut Context<Self>) {
        let Some(terminal) = self.terminals.get_mut(&terminal_id) else {
            return;
        };
        if terminal.key.is_none() {
            return;
        }
        terminal.save_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(200))
                .await;
            let input = this
                .read_with(cx, |this, _| {
                    let terminal = this.terminals.get(&terminal_id)?;
                    Some((terminal.key.clone()?, terminal.changes.clone()))
                })
                .ok()
                .flatten();
            let Some((key, all_changes)) = input else {
                return;
            };
            for changes in all_changes {
                let store = changes.store.clone();
                let key = key.clone();
                let saved = cx
                    .background_spawn(async move {
                        pending::save(&changes.store, &key, &changes.directory, &changes.files)
                    })
                    .await;
                match saved {
                    Ok(trees) => {
                        this.update(cx, |this, _| {
                            if let Some(changes) = this
                                .terminals
                                .get_mut(&terminal_id)
                                .and_then(|terminal| terminal.changes_in_mut(store.store_dir()))
                            {
                                changes.trees = trees;
                            }
                        })
                        .ok();
                    }
                    Err(error) => log::warn!("failed to record pending changes: {error:#}"),
                }
            }
        }));
    }

    /// SOLO: Checks the terminal's pending files again, a moment from now:
    /// which still differ from what's accepted. While no review is open and
    /// no turn is under way, what the user did since is taken in too (see
    /// [`pending::reconcile`]).
    pub(crate) fn refresh_pending(&mut self, terminal_id: EntityId, cx: &mut Context<Self>) {
        let Some(terminal) = self.terminals.get_mut(&terminal_id) else {
            return;
        };
        terminal.refresh_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(PENDING_REFRESH_DELAY).await;
            let input = this
                .read_with(cx, |this, _| {
                    let terminal = this.terminals.get(&terminal_id)?;
                    let active = this.active_turns(terminal_id);
                    let take_in = terminal.reviews_open == 0 && active.is_empty();
                    Some((terminal.changes.clone(), active, take_in))
                })
                .ok()
                .flatten();
            let Some((all_changes, active, take_in)) = input else {
                return;
            };
            for changes in all_changes {
                let store = changes.store.clone();
                let files = changes.files.clone();
                let active = active.clone();
                let (files, pending) = cx
                    .background_spawn(async move {
                        let locked = locked_files(&store, files.keys(), &active);
                        pending::reconcile(&store, &files, &locked, take_in)
                    })
                    .await;
                this.update(cx, |this, cx| {
                    let Some(terminal) = this.terminals.get_mut(&terminal_id) else {
                        return;
                    };
                    let Some(current) = terminal.changes_in_mut(changes.store.store_dir()) else {
                        return;
                    };
                    // Changed meanwhile: checked again then.
                    if current.files != changes.files || current.revision != changes.revision {
                        return;
                    }
                    let changed = current.files != files;
                    current.pending = pending;
                    if changed {
                        current.files = files;
                        current.revision += 1;
                        this.save_pending(terminal_id, cx);
                    }
                    if let Some(view) =
                        this.terminals.get(&terminal_id).and_then(|t| t.view.clone())
                    {
                        update_tab(&view, cx);
                    }
                    cx.notify();
                })
                .ok();
            }
        }));
    }

    /// SOLO: Checks the pending files again when they change on disk.
    fn watch_pending_files(&mut self, terminal_id: EntityId, cx: &mut Context<Self>) {
        let Some(terminal) = self.terminals.get_mut(&terminal_id) else {
            return;
        };
        if terminal._watch_files.is_some() {
            return;
        }
        let Some(project) = terminal
            .project
            .as_ref()
            .and_then(|project| project.upgrade())
        else {
            return;
        };
        terminal._watch_files = Some(cx.subscribe(&project, move |this, project, event, cx| {
            let project::Event::WorktreeUpdatedEntries(worktree_id, updates) = event else {
                return;
            };
            let Some(terminal) = this.terminals.get(&terminal_id) else {
                return;
            };
            if terminal.changes.iter().all(|changes| changes.files.is_empty()) {
                return;
            }
            let Some(worktree) = project.read(cx).worktree_for_id(*worktree_id, cx) else {
                return;
            };
            let worktree = worktree.read(cx);
            let touched = updates.iter().any(|(path, _, _)| {
                let abs_path = worktree.absolutize(path);
                terminal.changes.iter().any(|changes| {
                    changes
                        .files
                        .keys()
                        .any(|path| is_same_path(&changes.store.abs_path(path), &abs_path))
                })
            });
            if touched {
                this.refresh_pending(terminal_id, cx);
            }
        }));
    }

    /// SOLO: A review page of the terminal opened: what earlier turns
    /// changed isn't accepted while it's open.
    pub(crate) fn review_opened(&mut self, terminal_id: EntityId) {
        self.terminals.entry(terminal_id).or_default().reviews_open += 1;
    }

    /// SOLO: A review page of the terminal closed: what was to be accepted
    /// meanwhile is now.
    pub(crate) fn review_closed(&mut self, terminal_id: EntityId, cx: &mut Context<Self>) {
        let Some(terminal) = self.terminals.get_mut(&terminal_id) else {
            return;
        };
        terminal.reviews_open = terminal.reviews_open.saturating_sub(1);
        if terminal.reviews_open > 0 {
            return;
        }
        if std::mem::take(&mut terminal.auto_keep_due) {
            self.auto_keep(terminal_id, cx);
        } else {
            self.refresh_pending(terminal_id, cx);
        }
    }

    /// SOLO: Accepts what only turns before the last finished one changed
    /// (see [`pending::auto_keep`]), as that turn's end would have, in the
    /// folder that turn was in.
    fn auto_keep(&mut self, terminal_id: EntityId, cx: &mut Context<Self>) {
        let Some(terminal) = self.terminals.get(&terminal_id) else {
            return;
        };
        let finished = terminal.finished_turns();
        let Some(last) = terminal
            .turns
            .back()
            .filter(|turn| turn.number == finished)
            .cloned()
        else {
            self.refresh_pending(terminal_id, cx);
            return;
        };
        let Some(changes) = terminal.changes_in(last.store.store_dir()).cloned() else {
            self.refresh_pending(terminal_id, cx);
            return;
        };
        let last = (last.before.clone(), last.after.clone(), last.changed);
        let active = self.active_turns(terminal_id);
        let view = terminal.view.clone();
        cx.spawn(async move |this, cx| {
            let store = changes.store.clone();
            let files = changes.files.clone();
            let kept = cx
                .background_spawn(async move {
                    let locked = locked_files(&store, files.keys(), &active);
                    pending::auto_keep_now(&store, &files, &locked, Some(&last))
                })
                .await;
            this.update(cx, |this, cx| {
                match kept {
                    Ok(files) => {
                        let Some(current) = this
                            .terminals
                            .get_mut(&terminal_id)
                            .and_then(|terminal| terminal.changes_in_mut(changes.store.store_dir()))
                        else {
                            return;
                        };
                        if current.files != changes.files {
                            return;
                        }
                        current.pending = files.keys().cloned().collect();
                        current.files = files;
                        current.revision += 1;
                        this.save_pending(terminal_id, cx);
                        this.refresh_pending(terminal_id, cx);
                    }
                    Err(error) => log::warn!("failed to accept earlier turns' changes: {error:#}"),
                }
                if let Some(view) = &view {
                    update_tab(view, cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// SOLO: The files at `paths` (relative to the work tree of the store
    /// at `store_dir`) are accepted as they are now: rolled back, say.
    pub(crate) fn accept_as_they_are(
        &mut self,
        terminal_id: EntityId,
        store_dir: &Path,
        paths: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(changes) = self
            .terminals
            .get(&terminal_id)
            .and_then(|terminal| terminal.changes_in(store_dir))
            .cloned()
        else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let store = changes.store.clone();
            let files = changes.files.clone();
            let updates = cx
                .background_spawn(async move {
                    paths
                        .into_iter()
                        .filter(|path| files.contains_key(path))
                        .map(|path| {
                            let current = pending::current_version(&store, &store.abs_path(&path));
                            let file = files.get(&path).map(|file| PendingFile {
                                accepted: current.clone(),
                                left: current,
                                ..file.clone()
                            });
                            (path, file, false)
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            this.update(cx, |this, cx| {
                let store_dir = changes.store.store_dir();
                this.update_pending_files(terminal_id, store_dir, updates, cx);
                if let Some(changes) = this
                    .terminals
                    .get_mut(&terminal_id)
                    .and_then(|terminal| terminal.changes_in_mut(store_dir))
                {
                    changes.revision += 1;
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

/// SOLO: The id of a terminal's view's terminal (`SOLO_TERMINAL_ID`),
/// which outlives Solo with the terminal (G59).
fn terminal_key(view: &TerminalView, cx: &App) -> Option<String> {
    view.terminal
        .read(cx)
        .shell_integration()
        .terminal_id()
        .filter(|id| !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric()))
        .map(str::to_string)
}

/// SOLO: The pending changes recorded for the terminal `key`, in each
/// folder its agent worked in: the store, the folder, the files (with what
/// changed while Solo was closed marked as such) and when the record was
/// written, newest first (G59).
fn restore_pending(
    storage_root: &Path,
    key: &str,
) -> Vec<(
    Arc<SnapshotStore>,
    PathBuf,
    BTreeMap<String, PendingFile>,
    usize,
)> {
    let records = SnapshotStore::find_pending_records(storage_root, key);
    let count = records.len();
    let mut restored = Vec::new();
    for (ix, (path, contents)) in records.into_iter().enumerate() {
        let record: pending::Record = match serde_json::from_slice(&contents) {
            Ok(record) => record,
            Err(error) => {
                log::warn!("ignoring the unreadable record {path:?}: {error}");
                continue;
            }
        };
        let Some(store) = SnapshotStore::open(&record.directory, storage_root)
            .context("opening the snapshots")
            .log_err()
        else {
            continue;
        };
        if !path.starts_with(store.store_dir()) {
            log::warn!("the record {path:?} isn't in its folder's store");
            continue;
        }
        match pending::load(&store, &record) {
            Ok(files) => {
                let files = pending::changed_while_closed(&store, files);
                // Newest first: the folder worked in last counts as used last.
                restored.push((Arc::new(store), record.directory, files, count - ix));
            }
            Err(error) => {
                log::warn!("failed to read the pending changes in {path:?}: {error:#}");
                std::fs::remove_file(&path).ok();
            }
        }
    }
    restored
}

/// SOLO: Which of `paths` (relative to `store`'s work tree) a turn under
/// way or ending is changing: those its agent reported editing; for an
/// agent that doesn't report them, those that changed since the turn
/// started; all of them before that's known (G59).
pub(crate) fn locked_files<'a>(
    store: &SnapshotStore,
    paths: impl IntoIterator<Item = &'a String>,
    active: &[ActiveTurn],
) -> BTreeSet<String> {
    let paths: Vec<&String> = paths.into_iter().collect();
    let mut locked = BTreeSet::new();
    for turn in active {
        match (&turn.edited, &turn.before) {
            (Some(edited), _) => {
                let edited: Vec<PathBuf> = edited.iter().map(|path| comparable_path(path)).collect();
                for path in &paths {
                    let abs_path = comparable_path(&store.abs_path(path));
                    if edited.iter().any(|edited| is_same_path(edited, &abs_path)) {
                        locked.insert((*path).clone());
                    }
                }
            }
            (None, None) => locked.extend(paths.iter().map(|path| (*path).clone())),
            (None, Some((turn_store, before))) => {
                if turn_store.store_dir() != store.store_dir() {
                    continue;
                }
                let objects: Vec<String> = paths
                    .iter()
                    .map(|path| format!("{before}:{path}"))
                    .collect();
                let Ok(versions) = pending::load_versions(store, &objects) else {
                    locked.extend(paths.iter().map(|path| (*path).clone()));
                    continue;
                };
                for (path, before) in paths.iter().zip(versions) {
                    if pending::current_version(store, &store.abs_path(path)) != before {
                        locked.insert((*path).clone());
                    }
                }
            }
        }
    }
    locked
}

/// SOLO: `path` as the file system spells it (whatever the case, or 8.3
/// names, it was given in), to compare; a file that's gone by its directory.
fn comparable_path(path: &Path) -> PathBuf {
    if let Ok(path) = std::fs::canonicalize(path) {
        return path;
    }
    match (path.parent().map(std::fs::canonicalize), path.file_name()) {
        (Some(Ok(parent)), Some(name)) => parent.join(name),
        _ => path.to_path_buf(),
    }
}

/// SOLO: Whether two absolute paths name the same file, as reported (by an
/// agent) and as built from a snapshot: on Windows, whatever the case and
/// separators.
fn is_same_path(left: &Path, right: &Path) -> bool {
    if cfg!(windows) {
        let key = |path: &Path| {
            let text = path.to_string_lossy().replace('/', "\\").to_lowercase();
            text.strip_prefix(r"\\?\")
                .map(str::to_string)
                .unwrap_or(text)
        };
        key(left) == key(right)
    } else {
        left == right
    }
}

/// SOLO: The time a Codex turn's patches count in, in ms since the epoch.
#[derive(Clone, Copy, Debug)]
struct CodexWindow {
    from_ms: i64,
    to_ms: i64,
}

fn unix_ms(time: time::OffsetDateTime) -> i64 {
    (time.unix_timestamp_nanos() / 1_000_000) as i64
}

/// SOLO: The files Codex session `session`'s patches edited in `window`, as
/// its log events said, absolute (against `directory`, where it works);
/// `None` if its events don't come to Solo (say, the user's own telemetry
/// replaced Solo's), so they can't tell.
fn codex_edits(session: &str, window: CodexWindow, directory: &Path) -> Option<Vec<PathBuf>> {
    let receiver = agent_sessions::codex_otel::running_receiver()?;
    if !receiver.knows(session) {
        return None;
    }
    let mut paths: Vec<PathBuf> = Vec::new();
    for edit in receiver.edits(session, window.from_ms, window.to_ms) {
        let path = Path::new(&edit.path);
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            directory.join(path)
        };
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
    Some(paths)
}

fn update_tab(terminal_view: &WeakEntity<TerminalView>, cx: &mut App) {
    terminal_view
        .update(cx, |_, cx| cx.emit(ItemEvent::UpdateTab))
        .ok();
}

fn now() -> time::OffsetDateTime {
    let offset = time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC);
    time::OffsetDateTime::now_utc().to_offset(offset)
}

/// Why a turn has no snapshot, to tell the user; logs the error.
fn snapshot_unavailable_reason(error: &anyhow::Error) -> SharedString {
    let chinese = util::i18n::is_chinese();
    match error.downcast_ref::<SnapshotUnavailable>() {
        Some(SnapshotUnavailable::TooManyFiles { limit, .. }) => {
            log::info!("no snapshot for an agent turn: {error:#}");
            if chinese {
                format!("不是 git 仓库，且超过 {limit} 个文件").into()
            } else {
                format!("Not a git repository, and more than {limit} files").into()
            }
        }
        Some(SnapshotUnavailable::TooBroad(_)) => {
            log::info!("no snapshot for an agent turn: {error:#}");
            util::i18n::tr(
                "The folder is a whole drive or your home folder",
                "这个文件夹是整个磁盘或你的用户主目录",
            )
            .into()
        }
        Some(SnapshotUnavailable::NotLocal(_)) => {
            log::info!("no snapshot for an agent turn: {error:#}");
            util::i18n::tr(
                "The folder isn't on a local drive",
                "这个文件夹不在本地磁盘上",
            )
            .into()
        }
        None => {
            log::error!("failed to snapshot for an agent turn: {error:#}");
            util::i18n::tr("The snapshot failed (see the log)", "快照失败（详见日志）").into()
        }
    }
}

/// Notes the files in `directory` that `updates` changed, leaving out ignored
/// ones.
fn watch_changes(
    changes: &mut WatchedChanges,
    directory: &Path,
    worktree: &Worktree,
    updates: &UpdatedEntriesSet,
) {
    for (path, entry_id, change) in updates.iter() {
        let change = match change {
            PathChange::Added => FileChange::Added,
            PathChange::Updated | PathChange::AddedOrUpdated => FileChange::Modified,
            PathChange::Removed => FileChange::Deleted,
            PathChange::Loaded => continue,
        };
        if change == FileChange::Deleted {
            if worktree.is_path_ignored(path) {
                continue;
            }
        } else if !worktree
            .entry_for_id(*entry_id)
            .is_some_and(|entry| entry.is_file() && !entry.is_ignored)
        {
            continue;
        }
        let abs_path = worktree.absolutize(path);
        if !is_within(&abs_path, directory) {
            continue;
        }
        let previous = changes.files.get(&abs_path).copied();
        let change = match (previous, change) {
            // Created and deleted within the turn.
            (Some(FileChange::Added), FileChange::Deleted) => {
                changes.files.remove(&abs_path);
                continue;
            }
            (Some(FileChange::Added), _) => FileChange::Added,
            (Some(FileChange::Deleted), FileChange::Added) => FileChange::Modified,
            (_, change) => change,
        };
        if previous.is_none() && changes.files.len() >= MAX_WATCHED_FILES {
            changes.more = true;
            continue;
        }
        changes.files.insert(abs_path, change);
    }
}

/// The files of a turn's noted changes: removed directories, which removed
/// files inside them, left out.
fn changed_files(files: BTreeMap<PathBuf, FileChange>) -> Vec<(PathBuf, FileChange)> {
    let mut changed: Vec<(PathBuf, FileChange)> = files.into_iter().collect();
    // Sorted by path, so a directory comes right before what was in it.
    let removed_directories: Vec<usize> = changed
        .windows(2)
        .enumerate()
        .filter(|(_, pair)| {
            pair[0].1 == FileChange::Deleted
                && pair[1].1 == FileChange::Deleted
                && pair[1].0.starts_with(&pair[0].0)
        })
        .map(|(ix, _)| ix)
        .collect();
    for ix in removed_directories.into_iter().rev() {
        changed.remove(ix);
    }
    changed
}

/// Whether `path` is in `directory`; on Windows, whatever their case.
fn is_within(path: &Path, directory: &Path) -> bool {
    if path.starts_with(directory) {
        return true;
    }
    if cfg!(windows) {
        let lowercase = |path: &Path| PathBuf::from(path.to_string_lossy().to_lowercase());
        lowercase(path).starts_with(lowercase(directory))
    } else {
        false
    }
}

impl TerminalView {
    /// Where the agent running in this terminal works: where it said in its
    /// reports, else where the shell was when it started it. A turn keeps the
    /// directory it started in.
    fn agent_working_directory(&self, cx: &App) -> Option<PathBuf> {
        if let Some(directory) = self.agent_reported_directory() {
            return Some(directory.clone());
        }
        let terminal = self.terminal.read(cx);
        terminal
            .shell_integration()
            .blocks()
            .iter()
            .rev()
            .find(|block| block.is_running())
            .and_then(|block| block.working_directory.clone())
            .or_else(|| terminal.shell_integration().working_directory().cloned())
            .or_else(|| terminal.working_directory())
    }
}

/// The entry on an agent terminal's tab to review what it changed: how many
/// files have changes the user hasn't accepted (SOLO, G59).
pub(crate) fn render_tab_entry(view: &TerminalView, cx: &App) -> Option<AnyElement> {
    let terminal_view = view.self_handle.upgrade()?;
    let terminal_id = terminal_view.entity_id();
    let turns = AgentTurns::try_global(cx)?.read(cx);
    let count = turns.pending_file_count(terminal_id);
    if count == 0 {
        let turn = turns.offered_unsnapshotted_turn(terminal_id)?;
        return Some(render_unsnapshotted_tab_entry(terminal_view, turn.clone()));
    }
    // Only the count, as designed: what the last turn did to the errors is
    // on the review page.
    let label = pending_files_label(count);
    // As the title bar's update button: a pill of the accent color.
    let tint = cx.theme().colors().text_accent;
    Some(
        h_flex()
            .id(("agent-pending-changes", terminal_id.as_u64() as usize))
            .flex_none()
            .h(rems_from_px(20_f32))
            .pl(rems_from_px(6_f32))
            .pr(rems_from_px(8_f32))
            .gap(rems_from_px(4_f32))
            .rounded_full()
            .border_1()
            .bg(tint.opacity(0.14))
            .border_color(tint.opacity(0.28))
            .hover(move |style| style.bg(tint.opacity(0.18)).border_color(tint.opacity(0.40)))
            .cursor_pointer()
            .child(
                Icon::new(IconName::Diff)
                    .size(IconSize::XSmall)
                    .color(Color::Accent),
            )
            .child(Label::new(label).size(LabelSize::XSmall))
            .tooltip(Tooltip::text(util::i18n::tr(
                "Review the agent's changes you haven't accepted",
                "审阅 agent 还没被认可的改动",
            )))
            .on_click(move |_, window, cx| {
                cx.stop_propagation();
                let Some(workspace) = terminal_view.read(cx).workspace.upgrade() else {
                    return;
                };
                AgentTurnReview::open(terminal_view.clone(), workspace, window, cx);
            })
            .into_any_element(),
    )
}

/// SOLO: "N files to review" (G59).
fn pending_files_label(count: usize) -> String {
    if util::i18n::is_chinese() {
        format!("{count} 个文件待审")
    } else if count == 1 {
        "1 file to review".to_string()
    } else {
        format!("{count} files to review")
    }
}

fn changed_files_label(count: usize, more: bool) -> String {
    let more = if more { "+" } else { "" };
    if util::i18n::is_chinese() {
        format!("{count}{more} 个文件已修改")
    } else if count == 1 && more.is_empty() {
        "1 file changed".to_string()
    } else {
        format!("{count}{more} files changed")
    }
}

/// The entry on the tab of a terminal whose last turn couldn't be
/// snapshotted: a menu of the files it changed, which opens them.
fn render_unsnapshotted_tab_entry(
    terminal_view: Entity<TerminalView>,
    turn: Arc<UnsnapshottedTurn>,
) -> AnyElement {
    let terminal_id = terminal_view.entity_id();
    let label = changed_files_label(turn.changed.len(), turn.more);
    PopoverMenu::new(("agent-turn-files", terminal_id.as_u64() as usize))
        .trigger_with_tooltip(
            Button::new(
                ("agent-turn-file-list", terminal_id.as_u64() as usize),
                label,
            )
            .style(ButtonStyle::Tinted(ui::TintColor::Accent))
            .label_size(LabelSize::XSmall),
            Tooltip::text(util::i18n::tr(
                "The files the agent changed in its last turn",
                "agent 本轮改动的文件",
            )),
        )
        .menu(move |window, cx| {
            let workspace = terminal_view.read(cx).workspace.clone();
            Some(unsnapshotted_turn_menu(
                &turn,
                terminal_view.downgrade(),
                workspace,
                window,
                cx,
            ))
        })
        .into_any_element()
}

fn unsnapshotted_turn_menu(
    turn: &UnsnapshottedTurn,
    terminal_view: WeakEntity<TerminalView>,
    workspace: WeakEntity<Workspace>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<ContextMenu> {
    let chinese = util::i18n::is_chinese();
    ContextMenu::build(window, cx, |mut menu, _, _| {
        menu = menu
            .header(util::i18n::tr(
                "No diff or roll back for this turn",
                "这一轮没有差异和回滚",
            ))
            .label(turn.reason.clone())
            .separator();
        for (abs_path, change) in turn.changed.iter().take(MAX_LISTED_FILES) {
            let relative = abs_path
                .strip_prefix(&turn.directory)
                .unwrap_or(abs_path)
                .to_string_lossy()
                .into_owned();
            let marker = match change {
                FileChange::Added => "A",
                FileChange::Modified => "M",
                FileChange::Deleted => "D",
            };
            let label = format!("{marker}  {relative}");
            if *change == FileChange::Deleted {
                menu = menu.label(label);
                continue;
            }
            let abs_path = abs_path.clone();
            let workspace = workspace.clone();
            menu = menu.entry(label, None, move |window, cx| {
                workspace
                    .update(cx, |workspace, cx| {
                        workspace
                            .open_abs_path(
                                abs_path.clone(),
                                workspace::OpenOptions::default(),
                                window,
                                cx,
                            )
                            .detach_and_log_err(cx);
                    })
                    .ok();
            });
        }
        let unlisted = turn.changed.len().saturating_sub(MAX_LISTED_FILES);
        if unlisted > 0 || turn.more {
            let more = if turn.more { "+" } else { "" };
            menu = menu.label(if chinese {
                format!("还有 {unlisted}{more} 个文件")
            } else {
                format!("and {unlisted}{more} more files")
            });
        }
        menu.separator()
            .entry(util::i18n::tr("Dismiss", "忽略"), None, move |_, cx| {
                AgentTurns::global(cx).update(cx, |turns, cx| {
                    turns.dismiss_offer(terminal_view.entity_id(), cx)
                });
                update_tab(&terminal_view, cx);
            })
    })
}

impl AgentTurns {
    /// SOLO: Whether the terminal has turns or pending changes to review.
    pub(crate) fn has_something_to_review(&self, terminal: EntityId) -> bool {
        self.turns(terminal).next().is_some() || self.pending_file_count(terminal) > 0
    }

    /// SOLO: When the terminal's agent last changed something to review.
    fn last_change(&self, terminal: EntityId) -> Option<time::OffsetDateTime> {
        self.turns(terminal)
            .last()
            .map(|turn| turn.started_at)
            .or_else(|| {
                (self.pending_file_count(terminal) > 0)
                    .then_some(time::OffsetDateTime::UNIX_EPOCH)
            })
    }
}

/// Whether a terminal has recorded turns or pending changes to review.
pub(crate) fn has_turns(view: &TerminalView, cx: &App) -> bool {
    let Some(turns) = AgentTurns::try_global(cx) else {
        return false;
    };
    view.self_handle
        .upgrade()
        .is_some_and(|view| turns.read(cx).has_something_to_review(view.entity_id()))
}

fn review_agent_turn(
    workspace: &mut Workspace,
    _: &ReviewAgentTurn,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(turns) = AgentTurns::try_global(cx).cloned() else {
        return;
    };
    let has_turns = |view: &Entity<TerminalView>, cx: &App| {
        turns.read(cx).has_something_to_review(view.entity_id())
    };
    let active = workspace
        .active_item_as::<TerminalView>(cx)
        .or_else(|| {
            let panel = workspace.panel::<TerminalPanel>(cx)?;
            panel
                .read(cx)
                .active_pane
                .read(cx)
                .active_item()?
                .downcast::<TerminalView>()
        })
        .filter(|view| has_turns(view, cx));
    // Otherwise the terminal whose last turn was the latest.
    let terminal_view = active.or_else(|| {
        let mut views = workspace
            .items_of_type::<TerminalView>(cx)
            .collect::<Vec<_>>();
        if let Some(panel) = workspace.panel::<TerminalPanel>(cx) {
            for pane in panel.read(cx).center.panes() {
                views.extend(pane.read(cx).items_of_type::<TerminalView>());
            }
        }
        views
            .into_iter()
            .filter_map(|view| {
                let changed_at = turns.read(cx).last_change(view.entity_id())?;
                Some((changed_at, view))
            })
            .max_by_key(|(changed_at, _)| *changed_at)
            .map(|(_, view)| view)
    });
    let Some(terminal_view) = terminal_view else {
        return;
    };
    let workspace_entity = cx.entity();
    window.defer(cx, move |window, cx| {
        AgentTurnReview::open(terminal_view, workspace_entity, window, cx);
    });
}

/// The prompt of the latest Claude Code session in `directory`, from its
/// transcript, if it was written since `since`.
fn last_claude_prompt(directory: &Path, since: time::OffsetDateTime) -> Option<String> {
    let config_dir = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".claude")))?;
    let project_dir = config_dir
        .join("projects")
        .join(claude_project_name(directory));
    let since = std::time::SystemTime::from(since);
    let transcript = std::fs::read_dir(project_dir)
        .ok()?
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let path = entry.path();
            if path
                .extension()
                .is_none_or(|extension| extension != "jsonl")
            {
                return None;
            }
            let modified = entry.metadata().ok()?.modified().ok()?;
            (modified >= since).then_some((modified, path))
        })
        .max_by_key(|(modified, _)| *modified)?
        .1;
    let tail = read_tail(&transcript, 256 * 1024).ok()?;
    last_prompt_in_transcript(&tail)
}

/// How Claude Code names the directory of a project's transcripts.
fn claude_project_name(directory: &Path) -> String {
    directory
        .to_string_lossy()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '-'
            }
        })
        .collect()
}

fn read_tail(path: &Path, max_bytes: u64) -> std::io::Result<String> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let mut file = std::fs::File::open(path)?;
    let length = file.metadata()?.len();
    file.seek(SeekFrom::Start(length.saturating_sub(max_bytes)))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// The last `last-prompt` record of a Claude Code transcript.
fn last_prompt_in_transcript(transcript: &str) -> Option<String> {
    transcript.lines().rev().find_map(|line| {
        if !line.contains("\"last-prompt\"") {
            return None;
        }
        let record: serde_json::Value = serde_json::from_str(line).ok()?;
        if record.get("type")?.as_str()? != "last-prompt" {
            return None;
        }
        let prompt = record.get("lastPrompt")?.as_str()?.trim();
        (!prompt.is_empty()).then(|| prompt.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    #[allow(clippy::disallowed_methods, reason = "test setup")]
    fn git(directory: &Path, args: &[&str]) {
        let output = util::command::new_std_command("git")
            .current_dir(directory)
            .args(["-c", "user.name=Solo", "-c", "user.email=solo@example.com"])
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}");
    }

    #[gpui::test]
    async fn test_turns_record_what_changed(cx: &mut TestAppContext) {
        crate::tests::init_test(cx).await;
        cx.executor().allow_parking();
        let temp = tempfile::tempdir().unwrap();
        let repository = temp.path().join("repo");
        std::fs::create_dir_all(&repository).unwrap();
        git(&repository, &["init", "--quiet"]);
        for name in ["a.txt", "b.txt", "c.txt"] {
            std::fs::write(repository.join(name), "before\n").unwrap();
        }
        git(&repository, &["add", "."]);
        git(&repository, &["commit", "--quiet", "-m", "initial"]);

        let terminal_view = cx.add_window(|window, cx| {
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
            TerminalView::new(
                terminal,
                WeakEntity::new_invalid(),
                None,
                WeakEntity::new_invalid(),
                window,
                cx,
            )
        });
        let terminal_view = terminal_view.root(cx).unwrap();
        let turns = cx.update(|cx| AgentTurns::global(cx));
        turns.update(cx, |turns, _| {
            turns.storage_root = Some(temp.path().join("storage"))
        });
        let terminal_id = terminal_view.entity_id();
        let run_turn = |edit: &dyn Fn(), cx: &mut TestAppContext| {
            turns.update(cx, |turns, cx| {
                turns.start_turn(
                    terminal_view.clone(),
                    Some(repository.clone()),
                    None,
                    WeakEntity::new_invalid(),
                    DiagnosticsBefore::default(),
                    cx,
                )
            });
            cx.run_until_parked();
            edit();
            turns.update(cx, |turns, cx| {
                turns.end_turn(terminal_view.clone(), None, cx)
            });
            cx.run_until_parked();
        };

        // A turn that changes three files, one that changes none, and one more.
        run_turn(
            &|| {
                std::fs::write(repository.join("a.txt"), "first\n").unwrap();
                std::fs::remove_file(repository.join("b.txt")).unwrap();
                std::fs::write(repository.join("new.txt"), "new\n").unwrap();
            },
            cx,
        );
        run_turn(&|| {}, cx);
        run_turn(
            &|| std::fs::write(repository.join("c.txt"), "second\n").unwrap(),
            cx,
        );

        turns.read_with(cx, |turns, _| {
            let recorded: Vec<(usize, Vec<String>)> = turns
                .turns(terminal_id)
                .map(|turn| {
                    let mut paths: Vec<String> =
                        turn.changed.iter().map(|file| file.path.clone()).collect();
                    paths.sort();
                    (turn.number, paths)
                })
                .collect();
            assert_eq!(
                recorded,
                vec![
                    (
                        1,
                        vec![
                            "a.txt".to_string(),
                            "b.txt".to_string(),
                            "new.txt".to_string()
                        ]
                    ),
                    (3, vec!["c.txt".to_string()]),
                ]
            );
            assert_eq!(
                turns.offered_turn(terminal_id).map(|turn| turn.number),
                Some(3)
            );
        });

        // Rolling back to before the first turn, and undoing it.
        let first = turns.read_with(cx, |turns, _| turns.turn(terminal_id, 1).cloned().unwrap());
        let restored = first.store.restore(&first.before).unwrap();
        for name in ["a.txt", "b.txt", "c.txt"] {
            assert_eq!(
                std::fs::read_to_string(repository.join(name)).unwrap(),
                "before\n"
            );
        }
        assert!(!repository.join("new.txt").exists());
        first.store.restore(&restored.undo).unwrap();
        assert_eq!(
            std::fs::read_to_string(repository.join("c.txt")).unwrap(),
            "second\n"
        );
        assert!(!repository.join("b.txt").exists());
        assert!(repository.join("new.txt").exists());
    }

    /// A large untracked archive isn't snapshotted, and the turn says so;
    /// past the turns kept, the store stops growing (G51).
    #[gpui::test]
    async fn test_large_files_and_old_turns_dont_grow_the_store(cx: &mut TestAppContext) {
        crate::tests::init_test(cx).await;
        cx.executor().allow_parking();
        let temp = tempfile::tempdir().unwrap();
        let repository = temp.path().join("repo");
        std::fs::create_dir_all(&repository).unwrap();
        git(&repository, &["init", "--quiet"]);
        std::fs::write(repository.join("a.txt"), "before\n").unwrap();
        git(&repository, &["add", "."]);
        git(&repository, &["commit", "--quiet", "-m", "initial"]);

        let terminal_view = cx.add_window(|window, cx| {
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
            TerminalView::new(
                terminal,
                WeakEntity::new_invalid(),
                None,
                WeakEntity::new_invalid(),
                window,
                cx,
            )
        });
        let terminal_view = terminal_view.root(cx).unwrap();
        let turns = cx.update(|cx| AgentTurns::global(cx));
        let storage = temp.path().join("storage");
        turns.update(cx, |turns, _| {
            turns.storage_root = Some(storage.clone());
            turns.max_untracked_file_bytes = Some(100 * 1024);
        });
        let terminal_id = terminal_view.entity_id();
        let run_turn = |edit: &dyn Fn(), cx: &mut TestAppContext| {
            turns.update(cx, |turns, cx| {
                turns.start_turn(
                    terminal_view.clone(),
                    Some(repository.clone()),
                    None,
                    WeakEntity::new_invalid(),
                    DiagnosticsBefore::default(),
                    cx,
                )
            });
            cx.run_until_parked();
            edit();
            turns.update(cx, |turns, cx| {
                turns.end_turn(terminal_view.clone(), None, cx)
            });
            cx.run_until_parked();
        };
        // Bytes that don't compress.
        let noise = |length: usize, seed: u64| -> Vec<u8> {
            let mut state = seed;
            (0..length)
                .map(|_| {
                    state = state
                        .wrapping_mul(6364136223846793005)
                        .wrapping_add(1442695040888963407);
                    (state >> 33) as u8
                })
                .collect()
        };
        let store_size = || {
            let mut total = 0;
            let mut directories = vec![storage.clone()];
            while let Some(directory) = directories.pop() {
                for entry in std::fs::read_dir(&directory).unwrap().flatten() {
                    if entry.file_type().unwrap().is_dir() {
                        directories.push(entry.path());
                    } else {
                        total += entry.metadata().unwrap().len();
                    }
                }
            }
            total
        };

        // The agent only unpacks an archive: the turn is recorded all the
        // same, with the archive named and not stored.
        run_turn(
            &|| std::fs::write(repository.join("build (1).zip"), noise(600 * 1024, 1)).unwrap(),
            cx,
        );
        turns.read_with(cx, |turns, _| {
            let turn = turns.turn(terminal_id, 1).expect("the turn is recorded");
            assert!(turn.changed.is_empty());
            assert_eq!(turn.large_files, vec!["build (1).zip".to_string()]);
        });
        assert!(store_size() < 100 * 1024, "{}", store_size());

        // 25 turns that each write 32 KB that doesn't compress.
        const SIZE: usize = 32 * 1024;
        let mut sizes = Vec::new();
        for turn in 0..25u64 {
            run_turn(
                &|| std::fs::write(repository.join("data.bin"), noise(SIZE, 100 + turn)).unwrap(),
                cx,
            );
            sizes.push(store_size());
        }
        turns.read_with(cx, |turns, _| {
            assert_eq!(turns.turns(terminal_id).count(), MAX_TURNS);
            assert!(turns.turn(terminal_id, 1).is_none());
        });
        // What the 20 turns kept need: 21 versions of the file, and slack.
        let last = *sizes.last().unwrap();
        assert!(
            last < 21 * SIZE as u64 + 200 * 1024,
            "the store keeps growing: {sizes:?}"
        );
        assert!(
            last <= sizes[20] + 64 * 1024,
            "the store keeps growing: {sizes:?}"
        );
        // The oldest turn kept can still be shown and rolled back.
        let oldest = turns.read_with(cx, |turns, _| {
            turns.turns(terminal_id).next().cloned().unwrap()
        });
        assert_eq!(
            oldest
                .store
                .file_contents(&oldest.after, "data.bin")
                .unwrap(),
            noise(SIZE, 100 + oldest.number as u64 - 2)
        );
    }

    /// SOLO: What the user changes that doesn't touch the agent's changes,
    /// and what they commit, isn't pending; the pending changes are there
    /// after a restart (G59).
    #[gpui::test]
    async fn test_pending_changes_leave_out_the_users_and_outlive_solo(cx: &mut TestAppContext) {
        crate::tests::init_test(cx).await;
        cx.executor().allow_parking();
        let temp = tempfile::tempdir().unwrap();
        let repository = temp.path().join("repo");
        std::fs::create_dir_all(&repository).unwrap();
        git(&repository, &["init", "--quiet"]);
        git(&repository, &["config", "core.autocrlf", "false"]);
        let lines = |changed: &[(usize, &str)]| -> String {
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
        };
        std::fs::write(repository.join("a.txt"), lines(&[])).unwrap();
        std::fs::write(repository.join("b.txt"), "b\n").unwrap();
        git(&repository, &["add", "."]);
        git(&repository, &["commit", "--quiet", "-m", "initial"]);

        let terminal_view = cx.add_window(|window, cx| {
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
            TerminalView::new(
                terminal,
                WeakEntity::new_invalid(),
                None,
                WeakEntity::new_invalid(),
                window,
                cx,
            )
        });
        let terminal_view = terminal_view.root(cx).unwrap();
        let terminal_id = terminal_view.entity_id();
        let storage = temp.path().join("storage");
        let turns = cx.update(|cx| AgentTurns::global(cx));
        const KEY: &str = "t1234567";
        turns.update(cx, |turns, _| {
            turns.storage_root = Some(storage.clone());
            // As its shell's SOLO_TERMINAL_ID would.
            turns.terminals.entry(terminal_id).or_default().key = Some(KEY.to_string());
        });
        let pending = |cx: &mut TestAppContext| {
            turns.read_with(cx, |turns, _| {
                turns
                    .pending_changes(terminal_id)
                    .map(|changes| changes.pending.iter().cloned().collect::<Vec<_>>())
                    .unwrap_or_default()
            })
        };
        let refresh = |cx: &mut TestAppContext| {
            turns.update(cx, |turns, cx| turns.refresh_pending(terminal_id, cx));
            cx.executor().advance_clock(PENDING_REFRESH_DELAY);
            cx.run_until_parked();
        };

        // The agent changes a.txt's second line and b.txt.
        turns.update(cx, |turns, cx| {
            turns.start_turn(
                terminal_view.clone(),
                Some(repository.clone()),
                None,
                WeakEntity::new_invalid(),
                DiagnosticsBefore::default(),
                cx,
            )
        });
        cx.run_until_parked();
        std::fs::write(repository.join("a.txt"), lines(&[(2, "TWO")])).unwrap();
        std::fs::write(repository.join("b.txt"), "B\n").unwrap();
        turns.update(cx, |turns, cx| {
            turns.end_turn(terminal_view.clone(), None, cx)
        });
        cx.run_until_parked();
        assert_eq!(pending(cx), vec!["a.txt".to_string(), "b.txt".to_string()]);

        // The user changes another line of a.txt: that's accepted, the
        // agent's line isn't.
        std::fs::write(
            repository.join("a.txt"),
            lines(&[(2, "TWO"), (9, "mine")]),
        )
        .unwrap();
        refresh(cx);
        turns.read_with(cx, |turns, _| {
            let changes = turns.pending_changes(terminal_id).unwrap();
            assert_eq!(
                changes.files["a.txt"].accepted,
                pending::Version::Text(lines(&[(9, "mine")]).into())
            );
        });
        assert_eq!(pending(cx), vec!["a.txt".to_string(), "b.txt".to_string()]);

        // The user commits b.txt as the agent left it: it's accepted.
        git(&repository, &["add", "b.txt"]);
        git(&repository, &["commit", "--quiet", "-m", "b"]);
        refresh(cx);
        assert_eq!(pending(cx), vec!["a.txt".to_string()]);

        // Recorded, for after a restart.
        cx.executor().advance_clock(Duration::from_millis(200));
        cx.run_until_parked();
        let mut restored = restore_pending(&storage, KEY);
        assert_eq!(restored.len(), 1, "recorded");
        let (_, _, restored, _) = restored.remove(0);
        let versions = |files: BTreeMap<String, PendingFile>| {
            files
                .into_iter()
                .map(|(path, file)| (path, (file.accepted, file.left)))
                .collect::<BTreeMap<_, _>>()
        };
        let files = turns.read_with(cx, |turns, _| {
            turns.pending_changes(terminal_id).unwrap().files.clone()
        });
        assert_eq!(versions(restored), versions(files));

        // While Solo is closed, the agent in the terminal (which kept
        // running) changes another line.
        let closed_left = lines(&[(2, "TWO"), (5, "FIVE"), (9, "mine")]);
        std::fs::write(repository.join("a.txt"), &closed_left).unwrap();

        // A new Solo: the terminal's view comes back with its id.
        let restarted = cx.new(|_| AgentTurns {
            storage_root: Some(storage.clone()),
            ..AgentTurns::default()
        });
        restarted.update(cx, |turns, cx| {
            turns.register_terminal(
                terminal_view.clone(),
                Some(KEY.to_string()),
                WeakEntity::new_invalid(),
                cx,
            )
        });
        let started = std::time::Instant::now();
        while restarted.read_with(cx, |turns, _| turns.pending_file_count(terminal_id)) == 0 {
            assert!(
                started.elapsed() < Duration::from_secs(30),
                "the pending changes weren't restored"
            );
            cx.run_until_parked();
            std::thread::sleep(Duration::from_millis(20));
        }
        cx.executor().advance_clock(PENDING_REFRESH_DELAY);
        cx.run_until_parked();
        // That's pending, as the agent's, not taken in as the user's.
        restarted.read_with(cx, |turns, _| {
            assert_eq!(turns.pending_file_count(terminal_id), 1);
            let file = &turns.pending_changes(terminal_id).unwrap().files["a.txt"];
            assert_eq!(
                file.accepted,
                pending::Version::Text(lines(&[(9, "mine")]).into())
            );
            assert_eq!(file.left, pending::Version::Text(closed_left.as_str().into()));
            assert_eq!(
                file.while_closed,
                Some((
                    pending::Version::Text(lines(&[(2, "TWO"), (9, "mine")]).into()),
                    pending::Version::Text(closed_left.as_str().into()),
                ))
            );
        });
    }

    fn display_terminal(cx: &mut TestAppContext) -> Entity<TerminalView> {
        let window = cx.add_window(|window, cx| {
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
            TerminalView::new(
                terminal,
                WeakEntity::new_invalid(),
                None,
                WeakEntity::new_invalid(),
                window,
                cx,
            )
        });
        window.root(cx).unwrap()
    }

    fn committed_repository(directory: &Path, files: &[(&str, &str)]) {
        std::fs::create_dir_all(directory).unwrap();
        git(directory, &["init", "--quiet"]);
        git(directory, &["config", "core.autocrlf", "false"]);
        for (name, text) in files {
            std::fs::write(directory.join(name), text).unwrap();
        }
        git(directory, &["add", "."]);
        git(directory, &["commit", "--quiet", "-m", "initial"]);
    }

    /// SOLO: Two turns that end close together: the second's end waits for
    /// the first's, and both turns' changes are pending (G59).
    #[gpui::test]
    async fn test_turn_ends_close_together_both_count(cx: &mut TestAppContext) {
        crate::tests::init_test(cx).await;
        cx.executor().allow_parking();
        let temp = tempfile::tempdir().unwrap();
        let repository = temp.path().join("repo");
        committed_repository(&repository, &[("a.txt", "a\n"), ("b.txt", "b\n")]);
        let terminal_view = display_terminal(cx);
        let terminal_id = terminal_view.entity_id();
        let turns = cx.update(|cx| AgentTurns::global(cx));
        turns.update(cx, |turns, _| {
            turns.storage_root = Some(temp.path().join("storage"))
        });
        let start = |cx: &mut TestAppContext| {
            turns.update(cx, |turns, cx| {
                turns.start_turn(
                    terminal_view.clone(),
                    Some(repository.clone()),
                    None,
                    WeakEntity::new_invalid(),
                    DiagnosticsBefore::default(),
                    cx,
                )
            });
            let started = std::time::Instant::now();
            while !turns.read_with(cx, |turns, _| turns.start_snapshot_taken(terminal_id)) {
                assert!(started.elapsed() < Duration::from_secs(30));
                cx.run_until_parked();
                std::thread::sleep(Duration::from_millis(20));
            }
        };

        // A review is open, so turn 1's changes aren't accepted at turn 2's
        // end.
        turns.update(cx, |turns, _| turns.review_opened(terminal_id));
        start(cx);
        std::fs::write(repository.join("a.txt"), "A\n").unwrap();
        std::fs::write(repository.join("b.txt"), "B\n").unwrap();
        // Turn 1 ends, and turn 2 starts and ends, before anything of turn
        // 1's end is recorded: turn 2's end must not cancel it.
        turns.update(cx, |turns, cx| {
            turns.end_turn(terminal_view.clone(), None, cx);
            turns.start_turn(
                terminal_view.clone(),
                Some(repository.clone()),
                None,
                WeakEntity::new_invalid(),
                DiagnosticsBefore::default(),
                cx,
            );
            turns.end_turn(terminal_view.clone(), None, cx);
        });
        let started = std::time::Instant::now();
        while !turns.read_with(cx, |turns, _| turns.active_turns(terminal_id).is_empty()) {
            assert!(started.elapsed() < Duration::from_secs(30));
            cx.run_until_parked();
            std::thread::sleep(Duration::from_millis(20));
        }
        turns.read_with(cx, |turns, _| {
            let numbers: Vec<usize> = turns.turns(terminal_id).map(|turn| turn.number).collect();
            assert_eq!(numbers, vec![1]);
            let changes = turns.pending_changes(terminal_id).unwrap();
            let paths: Vec<&String> = changes.files.keys().collect();
            assert_eq!(paths, vec!["a.txt", "b.txt"]);
        });
    }

    /// SOLO: The agent works in another folder: what's pending in the first
    /// stays, the tab counts both, and the review shows the latest (G59).
    #[gpui::test]
    async fn test_pending_changes_in_two_folders(cx: &mut TestAppContext) {
        crate::tests::init_test(cx).await;
        cx.executor().allow_parking();
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        committed_repository(&first, &[("a.txt", "a\n")]);
        committed_repository(&second, &[("x.txt", "x\n")]);
        let terminal_view = display_terminal(cx);
        let terminal_id = terminal_view.entity_id();
        let turns = cx.update(|cx| AgentTurns::global(cx));
        turns.update(cx, |turns, _| {
            turns.storage_root = Some(temp.path().join("storage"))
        });
        for (directory, name) in [(&first, "a.txt"), (&second, "x.txt")] {
            turns.update(cx, |turns, cx| {
                turns.start_turn(
                    terminal_view.clone(),
                    Some(directory.clone()),
                    None,
                    WeakEntity::new_invalid(),
                    DiagnosticsBefore::default(),
                    cx,
                )
            });
            cx.run_until_parked();
            std::fs::write(directory.join(name), "changed\n").unwrap();
            turns.update(cx, |turns, cx| {
                turns.end_turn(terminal_view.clone(), None, cx)
            });
            cx.run_until_parked();
        }
        turns.read_with(cx, |turns, _| {
            assert_eq!(turns.pending_file_count(terminal_id), 2);
            let current = turns.pending_changes(terminal_id).unwrap();
            assert_eq!(current.directory, second);
            assert_eq!(current.files.keys().collect::<Vec<_>>(), vec!["x.txt"]);
            let terminal = &turns.terminals[&terminal_id];
            let first_changes = terminal
                .changes
                .iter()
                .find(|changes| changes.directory == first)
                .expect("the first folder's changes stay");
            assert_eq!(first_changes.files.keys().collect::<Vec<_>>(), vec!["a.txt"]);
        });
    }

    /// A turn reported by an agent's hooks in a directory that can't be
    /// snapshotted: the tab lists the files the project saw change.
    #[gpui::test]
    async fn test_turn_without_a_snapshot_lists_changed_files(cx: &mut TestAppContext) {
        use project::Fs as _;

        crate::tests::init_test(cx).await;
        cx.update(init);
        cx.executor().allow_parking();
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("project");
        let agent_directory = root.join("agent");
        // On disk too, as the snapshot runs git there: a plain directory.
        std::fs::create_dir_all(&agent_directory).unwrap();
        for name in ["a.txt", "c.txt", "e.txt"] {
            std::fs::write(agent_directory.join(name), "before\n").unwrap();
        }
        let fs = project::FakeFs::new(cx.executor());
        fs.insert_tree(
            &root,
            serde_json::json!({
                ".git": {},
                ".gitignore": "ignored/\n",
                "outside.txt": "before\n",
                "agent": {
                    "a.txt": "before\n",
                    "c.txt": "before\n",
                    "e.txt": "before\n",
                    "sub": { "d.txt": "before\n" },
                    "ignored": { "x.txt": "before\n" },
                },
            }),
        )
        .await;
        let project = project::Project::test(fs.clone(), [root.as_path()], cx).await;
        cx.run_until_parked();

        let turns = cx.update(|cx| AgentTurns::global(cx));
        turns.update(cx, |turns, _| {
            turns.storage_root = Some(temp.path().join("storage"));
            // Fewer files than there are, so the snapshot fails.
            turns.max_files_outside_repository = Some(1);
        });
        let terminal = cx.new(|cx| {
            let mut terminal = terminal::TerminalBuilder::new_display_only(
                Default::default(),
                terminal::terminal_settings::AlternateScroll::On,
                None,
                0,
                cx.background_executor(),
                util::paths::PathStyle::local(),
            )
            .subscribe(cx);
            terminal.set_shell_nonce("n");
            terminal
        });
        let window = cx.add_window(|window, cx| {
            TerminalView::new(
                terminal.clone(),
                WeakEntity::new_invalid(),
                None,
                project.downgrade(),
                window,
                cx,
            )
        });
        let terminal_view = window.root(cx).unwrap();
        let terminal_id = terminal_view.entity_id();
        let write = |bytes: &[u8], cx: &mut TestAppContext| {
            terminal.update(cx, |terminal, cx| terminal.write_raw_output(bytes, cx));
            cx.run_until_parked();
        };
        let tab_entry = |cx: &mut TestAppContext| {
            terminal_view.read_with(cx, |view, cx| render_tab_entry(view, cx).is_some())
        };

        // Claude Code runs in the terminal, and its hooks report a prompt.
        write(b"\x1b]133;A\x07$ \x1b]133;B\x07", cx);
        write(b"claude\r\n\x1b]633;E;claude;n\x07\x1b]133;C\x07", cx);
        let directory = agent_directory.to_string_lossy().replace('%', "%25");
        write(
            format!("\x1b]6973;agent;nonce=n;prompt;1;;claude;{directory}\x07").as_bytes(),
            cx,
        );
        turns.read_with(cx, |turns, _| {
            let terminal = turns.terminals.get(&terminal_id).expect("a turn started");
            assert!(terminal.pending.is_some());
            assert_eq!(terminal.watches.len(), 1);
        });

        // What the agent does; and a file outside its directory, and an
        // ignored one, that change meanwhile.
        fs.save(
            &agent_directory.join("a.txt"),
            &"after\n".into(),
            Default::default(),
        )
        .await
        .unwrap();
        fs.insert_file(agent_directory.join("b.txt"), b"new\n".to_vec())
            .await;
        fs.remove_file(&agent_directory.join("c.txt"), Default::default())
            .await
            .unwrap();
        fs.remove_dir(
            &agent_directory.join("sub"),
            project::RemoveOptions {
                recursive: true,
                ignore_if_not_exists: false,
            },
        )
        .await
        .unwrap();
        // Created and deleted within the turn.
        fs.insert_file(agent_directory.join("temp.txt"), b"temp\n".to_vec())
            .await;
        cx.run_until_parked();
        fs.remove_file(&agent_directory.join("temp.txt"), Default::default())
            .await
            .unwrap();
        fs.insert_file(agent_directory.join("ignored/y.txt"), b"y\n".to_vec())
            .await;
        fs.save(
            &root.join("outside.txt"),
            &"after\n".into(),
            Default::default(),
        )
        .await
        .unwrap();
        cx.run_until_parked();
        // Nothing is offered while the turn is under way.
        assert!(!tab_entry(cx));

        write(b"\x1b]6973;agent;nonce=n;stop;2\x07", cx);
        cx.executor().advance_clock(WATCH_AFTER_TURN);
        cx.run_until_parked();

        turns.read_with(cx, |turns, _| {
            assert!(turns.offered_turn(terminal_id).is_none());
            let turn = turns
                .offered_unsnapshotted_turn(terminal_id)
                .expect("the turn's files are offered");
            assert_eq!(turn.number, 1);
            assert!(turn.reason.contains("git"), "{}", turn.reason);
            assert!(!turn.more);
            assert_eq!(
                turn.changed,
                vec![
                    (agent_directory.join("a.txt"), FileChange::Modified),
                    (agent_directory.join("b.txt"), FileChange::Added),
                    (agent_directory.join("c.txt"), FileChange::Deleted),
                    (
                        agent_directory.join("sub").join("d.txt"),
                        FileChange::Deleted
                    ),
                ]
            );
            assert_eq!(turns.last_turn_changed_files(terminal_id), Some(4));
            assert!(turns.terminals[&terminal_id].watches.is_empty());
        });
        assert!(tab_entry(cx));

        // Dismissed, the tab stops offering them.
        turns.update(cx, |turns, cx| turns.dismiss_offer(terminal_id, cx));
        assert!(!tab_entry(cx));
    }

    #[test]
    fn test_changed_files_leave_out_removed_directories() {
        let files = BTreeMap::from_iter([
            (PathBuf::from("dir"), FileChange::Deleted),
            (PathBuf::from("dir").join("a"), FileChange::Deleted),
            (PathBuf::from("dir").join("b"), FileChange::Deleted),
            (PathBuf::from("dir-2"), FileChange::Deleted),
            (PathBuf::from("file"), FileChange::Modified),
        ]);
        assert_eq!(
            changed_files(files),
            vec![
                (PathBuf::from("dir").join("a"), FileChange::Deleted),
                (PathBuf::from("dir").join("b"), FileChange::Deleted),
                (PathBuf::from("dir-2"), FileChange::Deleted),
                (PathBuf::from("file"), FileChange::Modified),
            ]
        );
    }

    #[test]
    fn test_is_within() {
        let directory = Path::new(if cfg!(windows) {
            r"C:\Work\Project"
        } else {
            "/work/project"
        });
        assert!(is_within(&directory.join("a.txt"), directory));
        assert!(!is_within(
            &directory.with_file_name("Project2").join("a"),
            directory
        ));
        if cfg!(windows) {
            assert!(is_within(Path::new(r"c:\work\project\a.txt"), directory));
        }
    }

    #[test]
    fn test_claude_project_name() {
        assert_eq!(
            claude_project_name(Path::new(r"D:\ZHarness")),
            "D--ZHarness"
        );
        assert_eq!(
            claude_project_name(Path::new("/home/me/my_project.v2")),
            "-home-me-my-project-v2"
        );
    }

    #[test]
    fn test_last_prompt_in_transcript() {
        let transcript = [
            r#"{"type":"last-prompt","lastPrompt":"first","sessionId":"s"}"#,
            r#"{"type":"user","message":{"content":"mentions \"last-prompt\""}}"#,
            r#"{"type":"last-prompt","lastPrompt":"  fix the bug\nin main  ","sessionId":"s"}"#,
            r#"{"type":"assistant","message":{}}"#,
            r#"{"type":"last-prom"#,
        ]
        .join("\n");
        assert_eq!(
            last_prompt_in_transcript(&transcript).as_deref(),
            Some("fix the bug\nin main")
        );
        assert_eq!(last_prompt_in_transcript("{}"), None);
    }
}
