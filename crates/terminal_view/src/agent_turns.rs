//! SOLO: what an AI agent in a terminal changed, turn by turn.
//!
//! When an agent starts a turn (its hooks report a prompt, or else its screen
//! shows it at work), its working directory is snapshotted (see
//! [`snapshot`]); when the turn ends, it's snapshotted again and the files
//! that changed are recorded. The terminal's tab then offers to review them
//! (see [`review`]), where hunks can be kept or rejected, commented on for the
//! agent, or the work tree rolled back to before any recorded turn. Agents
//! whose status is only guessed from their output have no turns.
//!
//! A directory that can't be snapshotted (not a repository and too big, say)
//! still gets a list of the files that changed during the turn, from the
//! project's file events, without diffs or roll back.

mod diagnostics;
mod review;
mod snapshot;

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use collections::HashMap;
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

use crate::{AgentTurnEvent, TerminalView, terminal_panel::TerminalPanel};
use diagnostics::{DiagnosticsBefore, TurnDiagnostics};
pub(crate) use review::AgentTurnReview;
use snapshot::{ChangedFile, FileChange, SnapshotStore, SnapshotUnavailable, TreeId};

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

pub(crate) fn init(cx: &mut App) {
    cx.observe_new(|_: &mut TerminalView, _, cx: &mut Context<TerminalView>| {
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
                    AgentTurns::global(cx)
                        .update(cx, |turns, cx| turns.end_turn(terminal_view, cx));
                }
            }
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
    /// The work tree when the turn ended, to tell the agent's changes from
    /// ones made since.
    pub after: TreeId,
    pub changed: Vec<ChangedFile>,
    /// What the turn did to the language servers' diagnostics.
    pub diagnostics: TurnDiagnostics,
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
    _end_task: Option<Task<()>>,
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
}

struct GlobalAgentTurns(Entity<AgentTurns>);

impl Global for GlobalAgentTurns {}

impl AgentTurns {
    pub(crate) fn global(cx: &mut App) -> Entity<Self> {
        if let Some(global) = cx.try_global::<GlobalAgentTurns>() {
            return global.0.clone();
        }
        let turns = cx.new(|_| AgentTurns::default());
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
        let storage_root = self
            .storage_root
            .clone()
            .unwrap_or_else(|| paths::data_dir().join("agent_turns"));
        let terminal = self.terminals.entry(terminal_id).or_default();
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
                            Arc::new(store)
                        }
                    };
                    let started = std::time::Instant::now();
                    let before = store.snapshot()?;
                    log::debug!(
                        "snapshotted {:?} for an agent turn in {:?}",
                        store.work_tree(),
                        started.elapsed()
                    );
                    anyhow::Ok(Arc::new(StartedTurn { store, before }))
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
        });
        // Keep the store for the next turn once it's open.
        cx.spawn(async move |this, cx| {
            if let Ok(started) = snapshot.await {
                this.update(cx, |this, _| {
                    if let Some(terminal) = this.terminals.get_mut(&terminal_id) {
                        terminal.stores.insert(directory, started.store.clone());
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    fn end_turn(&mut self, terminal_view: Entity<TerminalView>, cx: &mut Context<Self>) {
        let terminal_id = terminal_view.entity_id();
        let Some(terminal) = self.terminals.get_mut(&terminal_id) else {
            return;
        };
        let Some(pending) = terminal.pending.take() else {
            return;
        };
        // Earlier turns' watches belong to end tasks this one replaces.
        terminal
            .watches
            .retain(|watch| watch.number == pending.number);
        let terminal_view = terminal_view.downgrade();
        terminal._end_task = Some(cx.spawn(async move |this, cx| {
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
            let changes = cx
                .background_spawn({
                    let started = started.clone();
                    async move {
                        let after = started.store.snapshot()?;
                        let changed = started.store.changed_files(&started.before, &after)?;
                        let prompt = prompt_source
                            .and_then(|directory| last_claude_prompt(&directory, started_at));
                        anyhow::Ok((after, changed, prompt))
                    }
                })
                .await
                .log_err();
            let Some((after, changed, prompt)) = changes else {
                return;
            };
            // The files to have the language servers check: those still there.
            let changed_paths: Vec<PathBuf> = changed
                .iter()
                .filter(|file| file.change != FileChange::Deleted)
                .map(|file| started.store.abs_path(&file.path))
                .collect();
            let number = pending.number;
            let recorded = this.update(cx, |this, cx| {
                let Some(terminal) = this.terminals.get_mut(&terminal_id) else {
                    return false;
                };
                terminal.offer_last_turn = !changed.is_empty();
                let recorded = !changed.is_empty();
                if recorded {
                    terminal.turns.push_back(AgentTurn {
                        number: pending.number,
                        started_at,
                        directory: pending.directory,
                        prompt,
                        store: started.store.clone(),
                        before: started.before.clone(),
                        after,
                        changed,
                        diagnostics: TurnDiagnostics::Checking,
                    });
                    if terminal.turns.len() > MAX_TURNS {
                        terminal.turns.pop_front();
                    }
                }
                cx.notify();
                update_tab(&terminal_view, cx);
                recorded
            });
            if !matches!(recorded, Ok(true)) {
                return;
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

/// The entry on an agent terminal's tab to review its last turn: how many
/// files it changed.
pub(crate) fn render_tab_entry(view: &TerminalView, cx: &App) -> Option<AnyElement> {
    let terminal_view = view.self_handle.upgrade()?;
    let turns = AgentTurns::try_global(cx)?.read(cx);
    let Some(turn) = turns.offered_turn(terminal_view.entity_id()) else {
        let turn = turns.offered_unsnapshotted_turn(terminal_view.entity_id())?;
        return Some(render_unsnapshotted_tab_entry(terminal_view, turn.clone()));
    };
    let chinese = util::i18n::is_chinese();
    let mut label = changed_files_label(turn.changed.len(), false);
    // What the turn did to the errors, once the language servers checked.
    if let Some((added, fixed)) = turn.diagnostics.error_delta()
        && (added > 0 || fixed > 0)
    {
        label.push_str(&if chinese {
            format!(" · 错误 +{added} / −{fixed}")
        } else {
            format!(" · errors +{added} / −{fixed}")
        });
    }
    let number = turn.number;
    Some(
        Button::new(
            (
                "agent-turn-changes",
                terminal_view.entity_id().as_u64() as usize,
            ),
            label,
        )
        .style(ButtonStyle::Tinted(ui::TintColor::Accent))
        .label_size(LabelSize::XSmall)
        .tooltip(Tooltip::text(util::i18n::tr(
            "Review what the agent changed in its last turn",
            "查看 agent 本轮的改动",
        )))
        .on_click(move |_, window, cx| {
            cx.stop_propagation();
            let Some(workspace) = terminal_view.read(cx).workspace.upgrade() else {
                return;
            };
            AgentTurnReview::open(terminal_view.clone(), number, workspace, window, cx);
        })
        .into_any_element(),
    )
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
        menu.separator().entry(
            util::i18n::tr("Dismiss", "不再显示"),
            None,
            move |_, cx| {
                AgentTurns::global(cx).update(cx, |turns, cx| {
                    turns.dismiss_offer(terminal_view.entity_id(), cx)
                });
                update_tab(&terminal_view, cx);
            },
        )
    })
}

/// Whether a terminal has recorded turns to review.
pub(crate) fn has_turns(view: &TerminalView, cx: &App) -> bool {
    let Some(turns) = AgentTurns::try_global(cx) else {
        return false;
    };
    view.self_handle
        .upgrade()
        .is_some_and(|view| turns.read(cx).turns(view.entity_id()).next().is_some())
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
        turns.read(cx).turns(view.entity_id()).next().is_some()
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
                let started_at = turns.read(cx).turns(view.entity_id()).last()?.started_at;
                Some((started_at, view))
            })
            .max_by_key(|(started_at, _)| *started_at)
            .map(|(_, view)| view)
    });
    let Some(terminal_view) = terminal_view else {
        return;
    };
    let Some(number) = turns
        .read(cx)
        .turns(terminal_view.entity_id())
        .last()
        .map(|turn| turn.number)
    else {
        return;
    };
    let workspace_entity = cx.entity();
    window.defer(cx, move |window, cx| {
        AgentTurnReview::open(terminal_view, number, workspace_entity, window, cx);
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
            turns.update(cx, |turns, cx| turns.end_turn(terminal_view.clone(), cx));
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
        write(b"claude\r\n\x1b]633;E;claude\x07\x1b]133;C\x07", cx);
        let directory = agent_directory.to_string_lossy().replace('%', "%25");
        write(
            format!("\x1b]6973;agent;prompt;1;;claude;{directory}\x07").as_bytes(),
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

        write(b"\x1b]6973;agent;stop;2\x07", cx);
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
