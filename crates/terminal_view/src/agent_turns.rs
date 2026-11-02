//! SOLO: what an AI agent in a terminal changed, turn by turn.
//!
//! When an agent that reports through hooks starts a turn, its working
//! directory is snapshotted (see [`snapshot`]); when the turn ends, it's
//! snapshotted again and the files that changed are recorded. The terminal's
//! tab then offers to review them (see [`review`]), where hunks can be kept or
//! rejected, commented on for the agent, or the work tree rolled back to
//! before any recorded turn. Terminals without hooks have no turns.

mod review;
mod snapshot;

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use collections::HashMap;
use futures::FutureExt as _;
use futures::future::{BoxFuture, Shared};
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, EntityId, Global, Task, WeakEntity, Window,
    actions,
};
use ui::{Tooltip, prelude::*};
use util::ResultExt as _;
use workspace::{Workspace, item::ItemEvent};

use crate::{AgentTurnEvent, TerminalView, terminal_panel::TerminalPanel};
pub(crate) use review::AgentTurnReview;
use snapshot::{ChangedFile, SnapshotStore, TreeId};

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
                    AgentTurns::global(cx).update(cx, |turns, cx| {
                        turns.start_turn(terminal_view, directory, prompt_source, cx)
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
    pub changed: Vec<ChangedFile>,
}

struct StartedTurn {
    store: Arc<SnapshotStore>,
    before: TreeId,
}

struct PendingTurn {
    number: usize,
    started_at: time::OffsetDateTime,
    directory: PathBuf,
    prompt_source: Option<PathBuf>,
    snapshot: Shared<BoxFuture<'static, Option<Arc<StartedTurn>>>>,
}

#[derive(Default)]
struct TerminalTurns {
    turns_started: usize,
    /// Finished turns that changed files, oldest first.
    turns: VecDeque<AgentTurn>,
    pending: Option<PendingTurn>,
    /// Whether the last finished turn changed files, so its tab offers them.
    offer_last_turn: bool,
    /// Stores by directory, which keep their index between snapshots.
    stores: HashMap<PathBuf, Arc<SnapshotStore>>,
    _end_task: Option<Task<()>>,
}

#[derive(Default)]
pub(crate) struct AgentTurns {
    terminals: HashMap<EntityId, TerminalTurns>,
    /// Where snapshots are stored, if not in Solo's data directory.
    storage_root: Option<PathBuf>,
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

    /// The files the last turn changed, while its tab offers them.
    fn offered_turn(&self, terminal: EntityId) -> Option<&AgentTurn> {
        let turns = self.terminals.get(&terminal)?;
        if !turns.offer_last_turn || turns.pending.is_some() {
            return None;
        }
        turns.turns.back()
    }

    fn start_turn(
        &mut self,
        terminal_view: Entity<TerminalView>,
        directory: Option<PathBuf>,
        prompt_source: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        let Some(directory) = directory else {
            log::debug!("no working directory to snapshot for an agent turn");
            return;
        };
        let terminal = self.terminals.entry(terminal_view.entity_id()).or_default();
        terminal.turns_started += 1;
        terminal.offer_last_turn = false;
        let known_store = terminal.stores.get(&directory).cloned();
        let storage_root = self
            .storage_root
            .clone()
            .unwrap_or_else(|| paths::data_dir().join("agent_turns"));
        let snapshot = cx
            .background_spawn({
                let directory = directory.clone();
                async move {
                    let store = match known_store {
                        Some(store) => store,
                        None => Arc::new(SnapshotStore::open(&directory, &storage_root)?),
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
            .map(|result| result.log_err())
            .boxed()
            .shared();
        terminal.pending = Some(PendingTurn {
            number: terminal.turns_started,
            started_at: now(),
            directory: directory.clone(),
            prompt_source,
            snapshot: snapshot.clone(),
        });
        // Keep the store for the next turn once it's open.
        let terminal_id = terminal_view.entity_id();
        cx.spawn(async move |this, cx| {
            if let Some(started) = snapshot.await {
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
        let terminal_view = terminal_view.downgrade();
        terminal._end_task = Some(cx.spawn(async move |this, cx| {
            let Some(started) = pending.snapshot.await else {
                return;
            };
            let prompt_source = pending.prompt_source;
            let started_at = pending.started_at;
            let changes = cx
                .background_spawn({
                    let started = started.clone();
                    async move {
                        let after = started.store.snapshot()?;
                        let changed = started.store.changed_files(&started.before, &after)?;
                        let prompt = prompt_source
                            .and_then(|directory| last_claude_prompt(&directory, started_at));
                        anyhow::Ok((changed, prompt))
                    }
                })
                .await
                .log_err();
            let Some((changed, prompt)) = changes else {
                return;
            };
            this.update(cx, |this, cx| {
                let Some(terminal) = this.terminals.get_mut(&terminal_id) else {
                    return;
                };
                terminal.offer_last_turn = !changed.is_empty();
                if !changed.is_empty() {
                    terminal.turns.push_back(AgentTurn {
                        number: pending.number,
                        started_at,
                        directory: pending.directory,
                        prompt,
                        store: started.store.clone(),
                        before: started.before.clone(),
                        changed,
                    });
                    if terminal.turns.len() > MAX_TURNS {
                        terminal.turns.pop_front();
                    }
                }
                cx.notify();
                update_tab(&terminal_view, cx);
            })
            .ok();
        }));
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

impl TerminalView {
    /// Where the agent running in this terminal works: where the shell was
    /// when it started it.
    fn agent_working_directory(&self, cx: &App) -> Option<PathBuf> {
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
    let turn = turns.offered_turn(terminal_view.entity_id())?;
    let count = turn.changed.len();
    let label = if util::i18n::is_chinese() {
        format!("{count} 个文件已修改")
    } else if count == 1 {
        "1 file changed".to_string()
    } else {
        format!("{count} files changed")
    };
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
                turns.start_turn(terminal_view.clone(), Some(repository.clone()), None, cx)
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
