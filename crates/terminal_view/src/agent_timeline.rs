//! SOLO: the timeline of the AI agent session a terminal runs, turn by turn:
//! the user's prompt, the files the agent read and changed, the commands it
//! ran and its answer. Read from the agent's transcript (see the
//! `agent_sessions` crate) and kept up to date as the agent works.
//!
//! It opens as an item beside the terminal, rather than in a dock: answers are
//! Markdown with code and diagrams that need the width, each agent terminal
//! has its own timeline, and the left dock holds the task board.

use std::{
    cell::RefCell,
    ops::Range,
    path::{Path, PathBuf},
    rc::Rc,
    time::Duration,
};

use agent_sessions::{AgentSession, AgentSessionEvent, MatchedBy, Role, SessionState, ToolKind};
use collections::{HashMap, HashSet};
use editor::{Editor, EditorEvent};
use gpui::{
    AnyElement, App, AppContext as _, Context, Empty, Entity, EventEmitter, FocusHandle, Focusable,
    FollowMode, FontWeight, ListAlignment, ListOffset, ListState, Subscription, Task, WeakEntity,
    Window, actions, list,
};
use language::LanguageRegistry;
use markdown::{Markdown, MarkdownElement, MarkdownFont, MarkdownOptions, MarkdownStyle};
use std::sync::Arc;
use ui::{HighlightedLabel, Tooltip, WithScrollbar, prelude::*};
use util::{
    ResultExt as _,
    i18n::{is_chinese, tr},
    paths::PathWithPosition,
};
use workspace::{
    OpenOptions, OpenVisible, SplitDirection, Workspace,
    item::{Item, ItemEvent},
};

use crate::{TerminalView, agent_session::ChooseAgentSession};

actions!(
    terminal,
    [
        /// Shows the timeline of the AI agent session the terminal runs: each
        /// turn's prompt, the files read and changed, the commands run and the
        /// answer.
        ShowAgentTimeline,
    ]
);

/// How often the terminal is checked for a different session.
const SESSION_CHECK_INTERVAL: Duration = Duration::from_secs(3);
/// Characters of a prompt shown before it's cut short.
const MAX_PROMPT_CHARS: usize = 1200;

/// What a tool call in a turn did, as the timeline groups them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Section {
    Read,
    Changed,
    Commands,
}

impl Section {
    const ALL: [Section; 3] = [Section::Read, Section::Changed, Section::Commands];

    fn label(self) -> &'static str {
        match self {
            Section::Read => tr("Read", "读过的文件"),
            Section::Changed => tr("Changed", "改过的文件"),
            Section::Commands => tr("Commands", "执行的命令"),
        }
    }

    fn icon(self) -> IconName {
        match self {
            Section::Read => IconName::Eye,
            Section::Changed => IconName::Pencil,
            Section::Commands => IconName::ToolTerminal,
        }
    }
}

/// One entry of a section: a file read or changed, a search, or a command.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Step {
    /// The tool, as the agent names it.
    pub tool: String,
    /// A file's path, a search's pattern or a command.
    pub label: String,
    /// A command's description or where a search looked.
    pub detail: Option<String>,
    /// The file to open when clicked.
    pub path: Option<PathBuf>,
    /// The one-based line to open the file at.
    pub line: Option<u32>,
    /// Text whose line to open the file at, for edits: the line it's on is
    /// only known once the file is read.
    pub find_text: Option<String>,
    /// How many times the turn read or changed the file.
    pub count: usize,
    /// A search rather than a file read; its path may be a directory.
    pub search: bool,
    pub failed: bool,
}

/// A turn of the main conversation: a prompt and what the agent did for it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Turn {
    /// Counts from 1; 0 is what was recorded before the first prompt.
    pub number: usize,
    pub prompt: Option<String>,
    pub timestamp: Option<String>,
    pub read: Vec<Step>,
    pub changed: Vec<Step>,
    pub commands: Vec<Step>,
    /// The agent's last text in the turn.
    pub answer: Option<String>,
}

impl Turn {
    fn steps(&self, section: Section) -> &[Step] {
        match section {
            Section::Read => &self.read,
            Section::Changed => &self.changed,
            Section::Commands => &self.commands,
        }
    }
}

/// A row of the timeline's list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Row {
    Prompt {
        turn: usize,
    },
    Section {
        turn: usize,
        section: Section,
    },
    Step {
        turn: usize,
        section: Section,
        step: usize,
    },
    Answer {
        turn: usize,
    },
}

impl Row {
    fn turn(&self) -> usize {
        match self {
            Row::Prompt { turn }
            | Row::Section { turn, .. }
            | Row::Step { turn, .. }
            | Row::Answer { turn } => *turn,
        }
    }
}

/// The turns of a session from turn `first_number` on.
pub(crate) fn build_turns(state: &SessionState, first_number: usize) -> Vec<Turn> {
    let mut turns = Vec::new();
    let ends = (state.messages.len(), state.tool_calls.len());
    for number in first_number..=state.turn_starts.len() {
        let (start, prompt) = match number.checked_sub(1) {
            None => ((0, 0), None),
            Some(ix) => {
                let Some(turn_start) = state.turn_starts.get(ix) else {
                    break;
                };
                (
                    (turn_start.message_ix, turn_start.tool_call_ix),
                    state.messages.get(turn_start.message_ix),
                )
            }
        };
        let end = state
            .turn_starts
            .get(number)
            .map_or(ends, |next| (next.message_ix, next.tool_call_ix));
        let messages = state.messages.get(start.0..end.0).unwrap_or_default();
        let tool_calls = state.tool_calls.get(start.1..end.1).unwrap_or_default();
        let answer = messages
            .iter()
            .rev()
            .find(|message| message.role == Role::Assistant && !message.sidechain)
            .map(|message| message.text.clone());
        if number == 0 && answer.is_none() && tool_calls.is_empty() {
            continue;
        }
        let mut turn = Turn {
            number,
            prompt: prompt.map(|message| message.text.clone()),
            timestamp: prompt.and_then(|message| message.timestamp.clone()),
            read: Vec::new(),
            changed: Vec::new(),
            commands: Vec::new(),
            answer,
        };
        for call in tool_calls {
            add_tool_call(&mut turn, call);
        }
        turns.push(turn);
    }
    turns
}

fn add_tool_call(turn: &mut Turn, call: &agent_sessions::ToolCall) {
    let input_string = |name: &str| {
        call.input
            .get(name)
            .and_then(|value| value.as_str())
            .map(str::to_string)
    };
    let failed = call.outcome == Some(agent_sessions::ToolOutcome::Failed);
    let step = |label: String| Step {
        tool: call.name.clone(),
        label,
        detail: None,
        path: None,
        line: None,
        find_text: None,
        count: 1,
        search: false,
        failed,
    };
    match call.kind {
        ToolKind::Read => {
            let Some(path) = &call.file_path else { return };
            let line = call
                .input
                .get("offset")
                .and_then(|offset| offset.as_u64())
                .and_then(|offset| u32::try_from(offset).ok())
                .filter(|line| *line > 0);
            add_file_step(
                &mut turn.read,
                Step {
                    path: Some(path.clone()),
                    line,
                    ..step(path.to_string_lossy().into_owned())
                },
            );
        }
        ToolKind::Search => {
            let pattern = input_string("pattern");
            let path = input_string("path");
            let label = match (&pattern, &path) {
                (Some(pattern), _) => pattern.clone(),
                (None, Some(path)) => path.clone(),
                (None, None) => return,
            };
            turn.read.push(Step {
                detail: pattern.and(path.clone()),
                path: path.map(PathBuf::from),
                search: true,
                ..step(label)
            });
        }
        ToolKind::Write | ToolKind::Edit => {
            let Some(path) = &call.file_path else { return };
            let find_text = (call.kind == ToolKind::Edit)
                .then(|| {
                    input_string("old_string").or_else(|| {
                        call.input
                            .pointer("/edits/0/old_string")
                            .and_then(|value| value.as_str())
                            .map(str::to_string)
                    })
                })
                .flatten()
                .and_then(|old| first_line_to_find(&old));
            add_file_step(
                &mut turn.changed,
                Step {
                    path: Some(path.clone()),
                    line: (call.kind == ToolKind::Write).then_some(1),
                    find_text,
                    ..step(path.to_string_lossy().into_owned())
                },
            );
        }
        ToolKind::Execute => {
            let Some(command) = call.command.clone() else {
                return;
            };
            turn.commands.push(Step {
                detail: call.description.clone(),
                ..step(command)
            });
        }
        ToolKind::Fetch | ToolKind::Delegate | ToolKind::Other => {}
    }
}

/// Adds a file read or change, once per file: the first time says where to
/// open it.
fn add_file_step(steps: &mut Vec<Step>, step: Step) {
    if let Some(existing) = steps.iter_mut().find(|existing| existing.path == step.path) {
        existing.count += 1;
        existing.failed &= step.failed;
    } else {
        steps.push(step);
    }
}

/// The first line of an edit's old text worth looking for in the file.
fn first_line_to_find(old_text: &str) -> Option<String> {
    old_text
        .lines()
        .map(|line| line.trim().trim_end_matches('…').trim())
        .find(|line| line.chars().count() >= 3)
        .map(str::to_string)
}

/// The one-based line `text` first appears on in `contents`.
pub(crate) fn line_of(contents: &str, text: &str) -> Option<u32> {
    let offset = contents.find(text)?;
    let line = contents[..offset].matches('\n').count() + 1;
    u32::try_from(line).ok()
}

/// Where `query` appears in `text`: ignoring ASCII case for an ASCII query,
/// exactly otherwise. Byte ranges, on character boundaries.
pub(crate) fn match_ranges(text: &str, query: &str) -> Vec<Range<usize>> {
    let query = query.trim();
    if query.is_empty() || query.len() > text.len() {
        return Vec::new();
    }
    let mut ranges = Vec::new();
    if query.is_ascii() {
        let haystack = text.as_bytes();
        let needle = query.as_bytes();
        let mut start = 0;
        while start + needle.len() <= haystack.len() {
            if haystack[start..start + needle.len()].eq_ignore_ascii_case(needle) {
                ranges.push(start..start + needle.len());
                start += needle.len();
            } else {
                start += 1;
            }
        }
    } else {
        ranges.extend(
            text.match_indices(query)
                .map(|(start, matched)| start..start + matched.len()),
        );
    }
    ranges
}

fn contains(text: &str, query: &str) -> bool {
    !match_ranges(text, query).is_empty()
}

fn step_matches(step: &Step, query: &str) -> bool {
    contains(&step.label, query)
        || step
            .detail
            .as_deref()
            .is_some_and(|detail| contains(detail, query))
}

/// The rows showing `turns`: with a query, the turns it appears in with only
/// what matches; without, every turn, collapsed ones as just their prompt.
pub(crate) fn timeline_rows(turns: &[Turn], query: &str, collapsed: &HashSet<usize>) -> Vec<Row> {
    let searching = !query.trim().is_empty();
    let mut rows = Vec::new();
    for (turn_ix, turn) in turns.iter().enumerate() {
        let prompt_matches = turn
            .prompt
            .as_deref()
            .is_some_and(|prompt| contains(prompt, query));
        let answer_matches = turn
            .answer
            .as_deref()
            .is_some_and(|answer| contains(answer, query));
        let mut turn_rows = Vec::new();
        if !searching && collapsed.contains(&turn.number) {
            rows.push(Row::Prompt { turn: turn_ix });
            continue;
        }
        for section in Section::ALL {
            let steps: Vec<usize> = turn
                .steps(section)
                .iter()
                .enumerate()
                .filter(|(_, step)| !searching || step_matches(step, query))
                .map(|(ix, _)| ix)
                .collect();
            if steps.is_empty() {
                continue;
            }
            turn_rows.push(Row::Section {
                turn: turn_ix,
                section,
            });
            turn_rows.extend(steps.into_iter().map(|step| Row::Step {
                turn: turn_ix,
                section,
                step,
            }));
        }
        if turn.answer.is_some() && (!searching || answer_matches) {
            turn_rows.push(Row::Answer { turn: turn_ix });
        }
        if searching && !prompt_matches && turn_rows.is_empty() {
            continue;
        }
        rows.push(Row::Prompt { turn: turn_ix });
        rows.extend(turn_rows);
    }
    rows
}

/// A link in an answer: a web address, or a file with maybe a line.
#[derive(Debug, PartialEq)]
pub(crate) enum Link {
    Url(String),
    File { path: PathBuf, line: Option<u32> },
}

pub(crate) fn parse_link(link: &str, root: Option<&Path>) -> Link {
    let link = link.trim();
    let path_part = match link.strip_prefix("file://") {
        Some(path) => path.trim_start_matches('/'),
        None if link.contains("://") || link.starts_with("mailto:") => {
            return Link::Url(link.to_string());
        }
        None => link,
    };
    let (path_part, line) = match path_part.rsplit_once("#L") {
        Some((path, line)) => (
            path,
            line.split(|char: char| !char.is_ascii_digit())
                .next()
                .and_then(|line| line.parse().ok()),
        ),
        None => (path_part, None),
    };
    let parsed = PathWithPosition::parse_str(path_part);
    let line = line.or(parsed.row);
    let mut path = parsed.path;
    if path.is_relative()
        && let Some(root) = root
    {
        path = root.join(path);
    }
    Link::File { path, line }
}

/// The file inline code in an answer names, like `src/main.rs:12`, when it
/// looks like a path. Whether the file exists is left to the caller.
pub(crate) fn code_span_path(text: &str, root: Option<&Path>) -> Option<PathBuf> {
    let text = text.trim();
    if text.is_empty() || text.contains(char::is_whitespace) || text.contains("://") {
        return None;
    }
    let file_name = text.rsplit(['/', '\\']).next().unwrap_or(text);
    let has_extension = file_name
        .split(':')
        .next()
        .and_then(|name| name.rsplit_once('.'))
        .is_some_and(|(stem, extension)| {
            !stem.is_empty() && extension.chars().all(|char| char.is_ascii_alphanumeric())
        });
    if !has_extension && !text.contains(['/', '\\']) {
        return None;
    }
    match parse_link(text, root) {
        Link::File { path, .. } if path.is_absolute() => Some(path),
        _ => None,
    }
}

fn short_time(timestamp: &str) -> Option<String> {
    let time =
        time::OffsetDateTime::parse(timestamp, &time::format_description::well_known::Rfc3339)
            .ok()?;
    let offset = time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC);
    let time = time.to_offset(offset);
    Some(format!(
        "{:02}-{:02} {:02}:{:02}",
        u8::from(time.month()),
        time.day(),
        time.hour(),
        time.minute()
    ))
}

fn cut_short(text: &str, max_chars: usize) -> String {
    match text.char_indices().nth(max_chars) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_string(),
    }
}

impl TerminalView {
    pub(crate) fn show_agent_timeline(
        &mut self,
        _: &ShowAgentTimeline,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        if !self.has_run_agent(cx) {
            workspace.update(cx, |workspace, cx| {
                workspace.show_toast(
                    workspace::Toast::new(
                        workspace::notifications::NotificationId::unique::<ShowAgentTimeline>(),
                        tr(
                            "No AI agent has run in this terminal",
                            "这个终端里没有运行过 AI agent",
                        ),
                    )
                    .autohide(),
                    cx,
                )
            });
            return;
        }
        // This view is being updated here, so the timeline can't read it, and
        // splitting or switching tabs, which can call back into it, waits
        // until the update is over.
        let root = self.terminal.read(cx).working_directory();
        let terminal_view = cx.entity();
        window.defer(cx, move |window, cx| {
            AgentTimeline::open(terminal_view, root, workspace, window, cx);
        });
    }
}

pub(crate) struct AgentTimeline {
    terminal_view: WeakEntity<TerminalView>,
    terminal_id: gpui::EntityId,
    workspace: WeakEntity<Workspace>,
    /// Where relative paths in answers are, and what file paths are shown
    /// relative to.
    root: Option<PathBuf>,
    languages: Option<Arc<LanguageRegistry>>,
    session: Option<Entity<AgentSession>>,
    looking: bool,
    turns: Vec<Turn>,
    rows: Vec<Row>,
    collapsed: HashSet<usize>,
    query: String,
    /// Rendered answers by turn number.
    answers: HashMap<usize, Entity<Markdown>>,
    /// Whether files named in answers' inline code exist, checked once each.
    files_exist: Rc<RefCell<HashMap<PathBuf, bool>>>,
    list_state: ListState,
    search_editor: Entity<Editor>,
    focus_handle: FocusHandle,
    _session_subscription: Option<Subscription>,
    _search_subscription: Subscription,
    _watch: Task<()>,
    _open_task: Task<()>,
}

impl AgentTimeline {
    fn open(
        terminal_view: Entity<TerminalView>,
        root: Option<PathBuf>,
        workspace: Entity<Workspace>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let terminal_id = terminal_view.entity_id();
        workspace.update(cx, |workspace, cx| {
            let existing = workspace
                .items_of_type::<AgentTimeline>(cx)
                .find(|timeline| timeline.read(cx).terminal_id == terminal_id);
            if let Some(existing) = existing {
                workspace.activate_item(&existing, true, true, window, cx);
                return;
            }
            let languages = Some(workspace.project().read(cx).languages().clone());
            let workspace_handle = cx.weak_entity();
            let timeline = cx.new(|cx| {
                AgentTimeline::new(terminal_view, root, workspace_handle, languages, window, cx)
            });
            // Beside the terminal, to watch both as the agent works.
            if workspace.active_pane().read(cx).items_len() == 0 {
                workspace.add_item_to_active_pane(Box::new(timeline), None, true, window, cx);
            } else {
                workspace.split_item(SplitDirection::Right, Box::new(timeline), window, cx);
            }
        });
    }

    fn new(
        terminal_view: Entity<TerminalView>,
        root: Option<PathBuf>,
        workspace: WeakEntity<Workspace>,
        languages: Option<Arc<LanguageRegistry>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search_editor = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text(
                tr("Search this session…", "在整个会话中搜索…"),
                window,
                cx,
            );
            editor
        });
        let search_subscription = cx.subscribe(&search_editor, |this, editor, event, cx| {
            if let EditorEvent::BufferEdited = event {
                let query = editor.read(cx).text(cx);
                this.set_query(query, cx);
            }
        });
        let list_state = ListState::new(0, ListAlignment::Top, px(1000.));
        list_state.set_follow_mode(FollowMode::Tail);
        let mut this = Self {
            terminal_id: terminal_view.entity_id(),
            terminal_view: terminal_view.downgrade(),
            workspace,
            root,
            languages,
            session: None,
            looking: true,
            turns: Vec::new(),
            rows: Vec::new(),
            collapsed: HashSet::default(),
            query: String::new(),
            answers: HashMap::default(),
            files_exist: Rc::default(),
            list_state,
            search_editor,
            focus_handle: cx.focus_handle(),
            _session_subscription: None,
            _search_subscription: search_subscription,
            _watch: Task::ready(()),
            _open_task: Task::ready(()),
        };
        this._watch = this.watch_terminal(cx);
        this
    }

    /// Follows the session the terminal runs, which changes when another agent
    /// run starts or the user picks one.
    fn watch_terminal(&self, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            let mut first = true;
            loop {
                let find = this.update(cx, |this, cx| {
                    let terminal_view = this.terminal_view.upgrade()?;
                    Some(terminal_view.update(cx, |terminal_view, cx| {
                        match terminal_view.agent_session().cloned() {
                            Some(session) if !first => Task::ready(Some(session)),
                            _ => terminal_view.find_agent_session(cx),
                        }
                    }))
                });
                let find = match find {
                    Ok(Some(find)) => find,
                    // The terminal is gone; its session stays readable.
                    Ok(None) => {
                        this.update(cx, |this, cx| {
                            this.looking = false;
                            cx.notify();
                        })
                        .ok();
                        break;
                    }
                    Err(_) => break,
                };
                first = false;
                let session = find.await;
                let updated = this.update(cx, |this, cx| {
                    this.looking = false;
                    match session {
                        Some(session) => this.set_session(session, cx),
                        None => cx.notify(),
                    }
                });
                if updated.is_err() {
                    break;
                }
                cx.background_executor().timer(SESSION_CHECK_INTERVAL).await;
            }
        })
    }

    fn set_session(&mut self, session: Entity<AgentSession>, cx: &mut Context<Self>) {
        if self.session.as_ref() == Some(&session) {
            return;
        }
        self._session_subscription =
            Some(cx.subscribe(&session, |this, _, event, cx| match event {
                AgentSessionEvent::Updated { .. } => {
                    let from = this.turns.last().map_or(0, |turn| turn.number);
                    this.rebuild(from, cx);
                }
                AgentSessionEvent::Reloaded => this.rebuild(0, cx),
            }));
        self.session = Some(session);
        self.answers.clear();
        self.collapsed.clear();
        self.rebuild(0, cx);
        cx.emit(ItemEvent::UpdateTab);
    }

    /// Rebuilds the turns from turn `from_number` on.
    fn rebuild(&mut self, from_number: usize, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else {
            return;
        };
        let keep = self
            .turns
            .iter()
            .position(|turn| turn.number >= from_number)
            .unwrap_or(self.turns.len());
        self.turns.truncate(keep);
        let new_turns = build_turns(session.read(cx).state(), from_number);
        self.turns.extend(new_turns);
        for turn in &self.turns[keep..] {
            let Some(markdown) = self.answers.get(&turn.number).cloned() else {
                continue;
            };
            match &turn.answer {
                Some(answer) if markdown.read(cx).source().as_ref() != answer.as_str() => {
                    let answer = answer.clone();
                    markdown.update(cx, |markdown, cx| markdown.replace(answer, cx));
                    self.highlight_answer(&markdown, cx);
                }
                Some(_) => {}
                None => {
                    self.answers.remove(&turn.number);
                }
            }
        }
        self.refresh_rows(Some(keep), cx);
    }

    /// Updates the list's rows. Rows of turns from `changed_turn` on are laid
    /// out again even when they're the same rows, as their contents changed.
    fn refresh_rows(&mut self, changed_turn: Option<usize>, cx: &mut Context<Self>) {
        let rows = timeline_rows(&self.turns, &self.query, &self.collapsed);
        let old = &self.rows;
        let mut prefix = old
            .iter()
            .zip(&rows)
            .take_while(|(old, new)| old == new)
            .count();
        let mut suffix = 0;
        if let Some(changed_turn) = changed_turn {
            let first_changed = rows
                .iter()
                .position(|row| row.turn() >= changed_turn)
                .unwrap_or(rows.len());
            prefix = prefix.min(first_changed);
        } else {
            suffix = old[prefix..]
                .iter()
                .rev()
                .zip(rows[prefix..].iter().rev())
                .take_while(|(old, new)| old == new)
                .count();
        }
        self.list_state
            .splice(prefix..old.len() - suffix, rows.len() - prefix - suffix);
        self.rows = rows;
        cx.notify();
    }

    fn set_query(&mut self, query: String, cx: &mut Context<Self>) {
        if query.trim() == self.query.trim() {
            self.query = query;
            return;
        }
        self.query = query;
        let answers: Vec<_> = self.answers.values().cloned().collect();
        for markdown in answers {
            self.highlight_answer(&markdown, cx);
        }
        self.refresh_rows(None, cx);
        self.list_state.scroll_to(ListOffset::default());
    }

    fn highlight_answer(&self, markdown: &Entity<Markdown>, cx: &mut App) {
        let query = self.query.clone();
        markdown.update(cx, |markdown, cx| {
            let ranges = match_ranges(markdown.source(), &query);
            if ranges.is_empty() {
                markdown.clear_search_highlights(cx);
            } else {
                markdown.set_search_highlights(ranges, None, cx);
            }
        });
    }

    fn toggle_turn(&mut self, number: usize, cx: &mut Context<Self>) {
        if !self.collapsed.remove(&number) {
            self.collapsed.insert(number);
        }
        self.refresh_rows(None, cx);
    }

    fn set_all_collapsed(&mut self, collapse: bool, cx: &mut Context<Self>) {
        self.collapsed = if collapse {
            self.turns.iter().map(|turn| turn.number).collect()
        } else {
            HashSet::default()
        };
        self.refresh_rows(None, cx);
    }

    fn answer_markdown(
        &mut self,
        turn_ix: usize,
        cx: &mut Context<Self>,
    ) -> Option<Entity<Markdown>> {
        let turn = self.turns.get(turn_ix)?;
        if let Some(markdown) = self.answers.get(&turn.number) {
            return Some(markdown.clone());
        }
        let answer = turn.answer.clone()?;
        let number = turn.number;
        let languages = self.languages.clone();
        let markdown = cx.new(|cx| {
            Markdown::new_with_options(
                answer.into(),
                languages,
                None,
                MarkdownOptions {
                    render_mermaid_diagrams: true,
                    ..MarkdownOptions::default()
                },
                cx,
            )
        });
        self.highlight_answer(&markdown, cx);
        self.answers.insert(number, markdown.clone());
        Some(markdown)
    }

    fn open_link(&mut self, link: &str, window: &mut Window, cx: &mut Context<Self>) {
        match parse_link(link, self.root.as_deref()) {
            Link::Url(url) => cx.open_url(&url),
            Link::File { path, line } => self.open_file(path, line, None, window, cx),
        }
    }

    /// Opens a file in the editor at `line`, or at the line `find_text` is on,
    /// beside the timeline rather than over it.
    fn open_file(
        &mut self,
        path: PathBuf,
        line: Option<u32>,
        find_text: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let workspace = self.workspace.clone();
        let this = cx.entity();
        self._open_task = cx.spawn_in(window, async move |_, cx| {
            let line = match (line, find_text) {
                (Some(line), _) => Some(line),
                (None, Some(text)) => {
                    let path = path.clone();
                    cx.background_spawn(async move {
                        let contents = std::fs::read_to_string(&path).ok()?;
                        line_of(&contents, &text)
                    })
                    .await
                }
                (None, None) => None,
            };
            let open = workspace.update_in(cx, |workspace, window, cx| {
                let timeline_pane = workspace.pane_for(&this);
                let pane = workspace
                    .panes()
                    .iter()
                    .find(|pane| Some(*pane) != timeline_pane.as_ref())
                    .map(|pane| pane.downgrade());
                workspace.open_paths(
                    vec![path.clone()],
                    OpenOptions {
                        visible: Some(OpenVisible::None),
                        focus: Some(true),
                        ..OpenOptions::default()
                    },
                    pane,
                    window,
                    cx,
                )
            });
            let Ok(open) = open else { return };
            let item = match open.await.into_iter().next().flatten() {
                Some(Ok(item)) => item,
                Some(Err(error)) => {
                    log::warn!("failed to open {path:?} from the agent timeline: {error:#}");
                    return;
                }
                None => return,
            };
            let (Some(line), Some(editor)) = (line, item.downcast::<Editor>()) else {
                return;
            };
            editor
                .update_in(cx, |editor, window, cx| {
                    if let Some(buffer) = editor.buffer().read(cx).as_singleton() {
                        let point = buffer
                            .read(cx)
                            .snapshot()
                            .point_from_external_input(line.saturating_sub(1), 0);
                        editor.go_to_singleton_buffer_point(point, window, cx);
                    }
                })
                .log_err();
        });
    }

    fn choose_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(terminal_view) = self.terminal_view.upgrade() {
            terminal_view.update(cx, |terminal_view, cx| {
                terminal_view.choose_agent_session(&ChooseAgentSession, window, cx)
            });
        }
    }

    fn display_path(&self, path: &str) -> String {
        self.root
            .as_deref()
            .and_then(|root| Path::new(path).strip_prefix(root).ok())
            .filter(|relative| !relative.as_os_str().is_empty())
            .map_or_else(
                || path.to_string(),
                |relative| relative.to_string_lossy().into_owned(),
            )
    }

    fn render_row(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(row) = self.rows.get(ix).copied() else {
            return Empty.into_any_element();
        };
        match row {
            Row::Prompt { turn } => self.render_prompt(ix, turn, cx),
            Row::Section { turn, section } => self.render_section(turn, section),
            Row::Step {
                turn,
                section,
                step,
            } => self.render_step(ix, turn, section, step, cx),
            Row::Answer { turn } => self.render_answer(turn, window, cx),
        }
    }

    fn render_prompt(&self, row_ix: usize, turn_ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(turn) = self.turns.get(turn_ix) else {
            return Empty.into_any_element();
        };
        let number = turn.number;
        let collapsed = self.query.trim().is_empty() && self.collapsed.contains(&number);
        let title = if number == 0 {
            tr("Before the first prompt", "第一个提示之前").to_string()
        } else if is_chinese() {
            format!("第 {number} 轮")
        } else {
            format!("Turn {number}")
        };
        let counts = [
            (turn.read.len(), tr("read", "读")),
            (turn.changed.len(), tr("changed", "改")),
            (turn.commands.len(), tr("commands", "命令")),
        ]
        .into_iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, label)| format!("{count} {label}"))
        .collect::<Vec<_>>()
        .join(" · ");
        let prompt = turn
            .prompt
            .as_deref()
            .map(|prompt| cut_short(prompt.trim(), MAX_PROMPT_CHARS));
        let colors = cx.theme().colors();
        v_flex()
            .id(("agent-timeline-prompt", row_ix))
            .w_full()
            .mt_3()
            .px_3()
            .py_2()
            .gap_1()
            .rounded_md()
            .bg(colors.element_background)
            .border_1()
            .border_color(colors.border_variant)
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_turn(number, cx)))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Icon::new(if collapsed {
                            IconName::ChevronRight
                        } else {
                            IconName::ChevronDown
                        })
                        .size(IconSize::Small)
                        .color(Color::Muted),
                    )
                    .child(
                        Label::new(title)
                            .size(LabelSize::Small)
                            .weight(FontWeight::BOLD),
                    )
                    .children(
                        turn.timestamp.as_deref().and_then(short_time).map(|time| {
                            Label::new(time).size(LabelSize::Small).color(Color::Muted)
                        }),
                    )
                    .child(div().flex_1())
                    .when(!counts.is_empty(), |this| {
                        this.child(
                            Label::new(counts)
                                .size(LabelSize::Small)
                                .color(Color::Muted),
                        )
                    }),
            )
            .children(prompt.map(|prompt| {
                let ranges = match_ranges(&prompt, &self.query);
                h_flex()
                    .gap_2()
                    .items_start()
                    .child(
                        Icon::new(IconName::Person)
                            .size(IconSize::Small)
                            .color(Color::Accent),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(HighlightedLabel::from_ranges(prompt, ranges)),
                    )
            }))
            .into_any_element()
    }

    fn render_section(&self, turn_ix: usize, section: Section) -> AnyElement {
        let count = self
            .turns
            .get(turn_ix)
            .map_or(0, |turn| turn.steps(section).len());
        h_flex()
            .w_full()
            .pt_2()
            .pl_4()
            .gap_1p5()
            .child(
                Icon::new(section.icon())
                    .size(IconSize::Small)
                    .color(Color::Muted),
            )
            .child(
                Label::new(format!("{} ({count})", section.label()))
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            )
            .into_any_element()
    }

    fn render_step(
        &self,
        row_ix: usize,
        turn_ix: usize,
        section: Section,
        step_ix: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(step) = self
            .turns
            .get(turn_ix)
            .and_then(|turn| turn.steps(section).get(step_ix))
        else {
            return Empty.into_any_element();
        };
        let is_file = section != Section::Commands && !step.search;
        let label = if is_file {
            self.display_path(&step.label)
        } else {
            step.label.lines().next().unwrap_or_default().to_string()
        };
        let multiline = !is_file && step.label.lines().nth(1).is_some();
        let label = if multiline {
            format!("{label} …")
        } else {
            label
        };
        let ranges = match_ranges(&label, &self.query);
        let mut detail = match (is_file, &step.detail) {
            (false, Some(detail)) => Some(if section == Section::Read {
                self.display_path(detail)
            } else {
                detail.clone()
            }),
            _ => None,
        };
        if is_file && step.count > 1 {
            detail = Some(format!("×{}", step.count));
        }
        let clickable_path = step.path.clone();
        let tooltip = match (&clickable_path, multiline) {
            (Some(path), _) => Some(match step.line {
                Some(line) => format!("{}:{line}", path.display()),
                None => path.display().to_string(),
            }),
            (None, true) => Some(step.label.clone()),
            (None, false) => None,
        };
        let line = step.line;
        let find_text = step.find_text.clone();
        h_flex()
            .id(("agent-timeline-step", row_ix))
            .w_full()
            .pl_8()
            .pr_3()
            .py_0p5()
            .gap_2()
            .rounded_sm()
            .child(
                Label::new(step.tool.clone())
                    .size(LabelSize::XSmall)
                    .color(Color::Muted)
                    .buffer_font(cx),
            )
            .child(
                div().min_w_0().child(
                    HighlightedLabel::from_ranges(label, ranges)
                        .size(LabelSize::Small)
                        .single_line()
                        .truncate()
                        .when(!is_file, |label| label.buffer_font(cx))
                        .color(if is_file {
                            Color::Accent
                        } else {
                            Color::Default
                        }),
                ),
            )
            .children(detail.map(|detail| {
                Label::new(detail)
                    .size(LabelSize::XSmall)
                    .color(Color::Muted)
                    .single_line()
                    .truncate()
            }))
            .when(step.failed, |this| {
                this.child(
                    Icon::new(IconName::XCircle)
                        .size(IconSize::XSmall)
                        .color(Color::Error),
                )
            })
            .when_some(tooltip, |this, tooltip| {
                this.tooltip(Tooltip::text(tooltip))
            })
            .when_some(clickable_path, |this, path| {
                this.cursor_pointer()
                    .hover(|style| style.bg(cx.theme().colors().element_hover))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if path.is_file() {
                            this.open_file(path.clone(), line, find_text.clone(), window, cx);
                        }
                    }))
            })
            .into_any_element()
    }

    fn render_answer(
        &mut self,
        turn_ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(markdown) = self.answer_markdown(turn_ix, cx) else {
            return Empty.into_any_element();
        };
        let this = cx.weak_entity();
        let root = self.root.clone();
        let files_exist = self.files_exist.clone();
        v_flex()
            .w_full()
            .pt_2()
            .pl_4()
            .pr_3()
            .gap_1()
            .child(
                h_flex()
                    .gap_1p5()
                    .child(
                        Icon::new(IconName::Sparkle)
                            .size(IconSize::Small)
                            .color(Color::Muted),
                    )
                    .child(
                        Label::new(tr("Answer", "回答"))
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    ),
            )
            .child(
                div().pl_5().child(
                    MarkdownElement::new(
                        markdown,
                        MarkdownStyle::themed(MarkdownFont::Agent, window, cx),
                    )
                    .on_code_span_link(move |text, _| {
                        let path = code_span_path(text, root.as_deref())?;
                        let exists = *files_exist
                            .borrow_mut()
                            .entry(path)
                            .or_insert_with_key(|path| path.is_file());
                        exists.then(|| SharedString::from(text.to_string()))
                    })
                    .on_url_click(move |url, window, cx| {
                        this.update(cx, |this, cx| this.open_link(&url, window, cx))
                            .ok();
                    }),
                ),
            )
            .into_any_element()
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let session = self.session.as_ref().map(|session| session.read(cx));
        let title = session
            .and_then(|session| session.state().title.clone())
            .or_else(|| session.map(|session| session.location().session_id.clone()))
            .unwrap_or_default();
        let guessed = session.is_some_and(|session| {
            matches!(
                session.location().matched_by,
                MatchedBy::Recent { ambiguous: true }
            )
        });
        let searching = !self.query.trim().is_empty();
        let matching_turns = searching.then(|| {
            let mut turns: Vec<usize> = self.rows.iter().map(Row::turn).collect();
            turns.dedup();
            turns.len()
        });
        let all_collapsed = !self.turns.is_empty() && self.collapsed.len() >= self.turns.len();
        let colors = cx.theme().colors();
        v_flex()
            .w_full()
            .px_3()
            .py_2()
            .gap_2()
            .border_b_1()
            .border_color(colors.border)
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div().flex_1().min_w_0().child(
                            Label::new(title)
                                .weight(FontWeight::SEMIBOLD)
                                .single_line()
                                .truncate(),
                        ),
                    )
                    .when(guessed, |this| {
                        this.child(
                            Label::new(tr("guessed", "推测"))
                                .size(LabelSize::Small)
                                .color(Color::Warning),
                        )
                    })
                    .child(
                        Button::new("agent-timeline-choose", tr("Choose Session…", "选择会话…"))
                            .label_size(LabelSize::Small)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.choose_session(window, cx)),
                            ),
                    )
                    .child(
                        IconButton::new(
                            "agent-timeline-collapse",
                            if all_collapsed {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            },
                        )
                        .icon_size(IconSize::Small)
                        .tooltip(Tooltip::text(if all_collapsed {
                            tr("Expand all turns", "展开所有轮次")
                        } else {
                            tr("Collapse all turns", "折叠所有轮次")
                        }))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.set_all_collapsed(!all_collapsed, cx)
                        })),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .border_1()
                    .border_color(colors.border_variant)
                    .bg(colors.editor_background)
                    .child(
                        Icon::new(IconName::MagnifyingGlass)
                            .size(IconSize::Small)
                            .color(Color::Muted),
                    )
                    .child(div().flex_1().child(self.search_editor.clone()))
                    .children(matching_turns.map(|count| {
                        Label::new(if is_chinese() {
                            format!("{count} 轮匹配")
                        } else if count == 1 {
                            "1 turn".to_string()
                        } else {
                            format!("{count} turns")
                        })
                        .size(LabelSize::Small)
                        .color(Color::Muted)
                    })),
            )
    }

    fn render_empty(&self, cx: &mut Context<Self>) -> AnyElement {
        let message = match &self.session {
            Some(session) if !session.read(cx).is_loaded() => {
                tr("Reading the session…", "正在读取会话…")
            }
            Some(_) if !self.query.trim().is_empty() => {
                tr("Nothing in this session matches", "会话中没有匹配的内容")
            }
            Some(_) => tr("The session has no turns yet", "会话里还没有轮次"),
            None if self.looking => {
                tr("Looking for the agent's session…", "正在查找 agent 的会话…")
            }
            None => tr(
                "Couldn't tell which session the agent runs",
                "无法确定 agent 运行的是哪个会话",
            ),
        };
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_2()
            .child(Label::new(message).color(Color::Muted))
            .when(self.session.is_none() && !self.looking, |this| {
                this.child(
                    Button::new(
                        "agent-timeline-choose-empty",
                        tr("Choose Session…", "选择会话…"),
                    )
                    .on_click(cx.listener(|this, _, window, cx| this.choose_session(window, cx))),
                )
            })
            .into_any_element()
    }
}

impl EventEmitter<ItemEvent> for AgentTimeline {}

impl Focusable for AgentTimeline {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Item for AgentTimeline {
    type Event = ItemEvent;

    fn tab_icon(&self, _window: &Window, _cx: &App) -> Option<Icon> {
        Some(Icon::new(IconName::ListTree).color(Color::Muted))
    }

    fn tab_content_text(&self, _detail: usize, cx: &App) -> SharedString {
        let agent = self
            .session
            .as_ref()
            .map(|session| session.read(cx).state().agent().name());
        match agent {
            Some(agent) if is_chinese() => format!("{agent} 时间线").into(),
            Some(agent) => format!("{agent} Timeline").into(),
            None => tr("Agent Timeline", "Agent 时间线").into(),
        }
    }

    fn tab_tooltip_text(&self, cx: &App) -> Option<SharedString> {
        let session = self.session.as_ref()?.read(cx);
        Some(
            session
                .state()
                .title
                .clone()
                .unwrap_or_else(|| session.location().path.display().to_string())
                .into(),
        )
    }

    fn to_item_events(event: &ItemEvent, f: &mut dyn FnMut(ItemEvent)) {
        f(*event)
    }
}

impl Render for AgentTimeline {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .key_context("AgentTimeline")
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(cx.theme().colors().editor_background)
            .child(self.render_header(cx))
            .child(if self.rows.is_empty() {
                self.render_empty(cx)
            } else {
                div()
                    .flex_1()
                    .min_h_0()
                    .px_2()
                    .pb_3()
                    .child(
                        list(self.list_state.clone(), cx.processor(Self::render_row)).size_full(),
                    )
                    .vertical_scrollbar_for(&self.list_state, window, cx)
                    .into_any_element()
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_sessions::{AgentKind, Message, ToolCall, ToolOutcome, TranscriptEntry};

    fn prompt(text: &str) -> TranscriptEntry {
        TranscriptEntry::Message(Message {
            role: Role::User,
            text: text.into(),
            id: None,
            api_message_id: None,
            timestamp: Some("2026-11-01T13:44:36.229Z".into()),
            sidechain: false,
        })
    }

    fn answer(text: &str, api_message_id: &str) -> TranscriptEntry {
        TranscriptEntry::Message(Message {
            role: Role::Assistant,
            text: text.into(),
            id: None,
            api_message_id: Some(api_message_id.into()),
            timestamp: None,
            sidechain: false,
        })
    }

    fn tool(id: &str, name: &str, kind: ToolKind, input: serde_json::Value) -> TranscriptEntry {
        let file_path = input
            .get("file_path")
            .and_then(|path| path.as_str())
            .map(PathBuf::from);
        let command = input
            .get("command")
            .and_then(|command| command.as_str())
            .map(str::to_string);
        TranscriptEntry::ToolCall(ToolCall {
            id: id.into(),
            name: name.into(),
            kind,
            file_path,
            command,
            description: input
                .get("description")
                .and_then(|description| description.as_str())
                .map(str::to_string),
            input,
            timestamp: None,
            sidechain: false,
            outcome: None,
        })
    }

    fn session() -> SessionState {
        let mut state = SessionState::new(AgentKind::ClaudeCode);
        state.apply_all([
            tool(
                "t0",
                "Read",
                ToolKind::Read,
                serde_json::json!({"file_path": "/repo/early.rs"}),
            ),
            prompt("Fix the parser"),
            answer("Let me look.", "m1"),
            tool(
                "t1",
                "Read",
                ToolKind::Read,
                serde_json::json!({"file_path": "/repo/src/parser.rs", "offset": 40}),
            ),
            tool(
                "t2",
                "Read",
                ToolKind::Read,
                serde_json::json!({"file_path": "/repo/src/parser.rs", "offset": 90}),
            ),
            tool(
                "t3",
                "Grep",
                ToolKind::Search,
                serde_json::json!({"pattern": "fn parse_line", "path": "/repo/src"}),
            ),
            tool(
                "t4",
                "Edit",
                ToolKind::Edit,
                serde_json::json!({"file_path": "/repo/src/parser.rs", "old_string": "\n  let x = 1;\n"}),
            ),
            tool(
                "t5",
                "PowerShell",
                ToolKind::Execute,
                serde_json::json!({"command": "cargo test -p parser", "description": "Run the tests"}),
            ),
            TranscriptEntry::ToolResult {
                tool_call_id: "t5".into(),
                outcome: ToolOutcome::Failed,
            },
            answer("Fixed it:\n\n```mermaid\ngraph TD; A-->B\n```", "m2"),
            prompt("Now write docs"),
            tool(
                "t6",
                "Write",
                ToolKind::Write,
                serde_json::json!({"file_path": "/repo/README.md", "content": "..."}),
            ),
            answer("Done.", "m3"),
        ]);
        state
    }

    #[test]
    fn test_turns_group_tool_calls_by_prompt() {
        let turns = build_turns(&session(), 0);
        assert_eq!(
            turns.iter().map(|turn| turn.number).collect::<Vec<_>>(),
            [0, 1, 2]
        );

        let before = &turns[0];
        assert_eq!(before.prompt, None);
        assert_eq!(before.read.len(), 1);

        let fix = &turns[1];
        assert_eq!(fix.prompt.as_deref(), Some("Fix the parser"));
        // The same file read twice is one entry, opening where it was first read.
        assert_eq!(fix.read.len(), 2);
        assert_eq!(fix.read[0].path, Some(PathBuf::from("/repo/src/parser.rs")));
        assert_eq!(fix.read[0].line, Some(40));
        assert_eq!(fix.read[0].count, 2);
        assert_eq!(fix.read[1].tool, "Grep");
        assert_eq!(fix.read[1].label, "fn parse_line");
        assert_eq!(fix.read[1].detail.as_deref(), Some("/repo/src"));
        assert_eq!(fix.changed.len(), 1);
        assert_eq!(fix.changed[0].find_text.as_deref(), Some("let x = 1;"));
        assert_eq!(fix.commands.len(), 1);
        assert_eq!(fix.commands[0].label, "cargo test -p parser");
        assert_eq!(fix.commands[0].detail.as_deref(), Some("Run the tests"));
        assert!(fix.commands[0].failed);
        // Only the last text is the answer.
        assert!(
            fix.answer
                .as_deref()
                .is_some_and(|answer| answer.starts_with("Fixed it"))
        );

        let docs = &turns[2];
        assert_eq!(docs.changed[0].line, Some(1));
        assert_eq!(docs.answer.as_deref(), Some("Done."));

        // Rebuilding from a turn gives the same turns from there on.
        assert_eq!(build_turns(&session(), 2), turns[2..]);
    }

    #[test]
    fn test_a_session_without_activity_before_its_first_prompt() {
        let mut state = SessionState::new(AgentKind::ClaudeCode);
        state.apply_all([prompt("hi"), answer("hello", "m1")]);
        let turns = build_turns(&state, 0);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].number, 1);
        assert_eq!(turns[0].answer.as_deref(), Some("hello"));
    }

    #[test]
    fn test_rows_and_collapsing() {
        let turns = build_turns(&session(), 0);
        let rows = timeline_rows(&turns, "", &HashSet::default());
        assert_eq!(
            &rows[..3],
            [
                Row::Prompt { turn: 0 },
                Row::Section {
                    turn: 0,
                    section: Section::Read
                },
                Row::Step {
                    turn: 0,
                    section: Section::Read,
                    step: 0
                },
            ]
        );
        assert_eq!(rows.last(), Some(&Row::Answer { turn: 2 }));

        let collapsed = HashSet::from_iter([1]);
        let rows = timeline_rows(&turns, "", &collapsed);
        let turn_one: Vec<_> = rows.iter().filter(|row| row.turn() == 1).collect();
        assert_eq!(turn_one, [&Row::Prompt { turn: 1 }]);
    }

    #[test]
    fn test_search() {
        let turns = build_turns(&session(), 0);
        let collapsed = HashSet::from_iter([1, 2]);

        // Matches show with their turn's prompt, collapsed or not.
        let rows = timeline_rows(&turns, "CARGO TEST", &collapsed);
        assert_eq!(
            rows,
            [
                Row::Prompt { turn: 1 },
                Row::Section {
                    turn: 1,
                    section: Section::Commands
                },
                Row::Step {
                    turn: 1,
                    section: Section::Commands,
                    step: 0
                },
            ]
        );

        let rows = timeline_rows(&turns, "readme", &collapsed);
        assert_eq!(rows[0], Row::Prompt { turn: 2 });
        assert_eq!(rows.len(), 3);

        // A prompt that matches shows on its own.
        assert_eq!(
            timeline_rows(&turns, "write docs", &collapsed),
            [Row::Prompt { turn: 2 }]
        );
        assert_eq!(
            timeline_rows(&turns, "mermaid", &collapsed),
            [Row::Prompt { turn: 1 }, Row::Answer { turn: 1 }]
        );
        // Search details too: the directory searched.
        assert!(
            timeline_rows(&turns, "/repo/src", &collapsed).contains(&Row::Step {
                turn: 1,
                section: Section::Read,
                step: 1
            })
        );
        assert!(timeline_rows(&turns, "nowhere to be found", &collapsed).is_empty());
    }

    #[test]
    fn test_match_ranges() {
        assert_eq!(match_ranges("Parse the PARSER", "parse"), [0..5, 10..15]);
        assert_eq!(match_ranges("改过的文件和文件", "文件"), [9..15, 18..24]);
        assert_eq!(match_ranges("é parse", "PARSE"), [3..8]);
        assert!(match_ranges("anything", "  ").is_empty());
        assert!(match_ranges("ab", "abc").is_empty());
    }

    #[test]
    fn test_code_span_path() {
        let root = Path::new(if cfg!(windows) { "C:\\repo" } else { "/repo" });
        assert_eq!(
            code_span_path("core/object/object.h", Some(root)),
            Some(root.join("core/object/object.h"))
        );
        assert_eq!(
            code_span_path("main.rs:12", Some(root)),
            Some(root.join("main.rs"))
        );
        assert_eq!(code_span_path("Object::get_class", Some(root)), None);
        assert_eq!(code_span_path("cargo test", Some(root)), None);
        assert_eq!(code_span_path(".cpp", Some(root)), None);
        assert_eq!(code_span_path("main.rs", None), None);
        assert_eq!(code_span_path("https://example.com/a.rs", Some(root)), None);
    }

    #[test]
    fn test_line_of() {
        let contents = "one\ntwo\n  let x = 1;\nfour";
        assert_eq!(line_of(contents, "let x = 1;"), Some(3));
        assert_eq!(line_of(contents, "one"), Some(1));
        assert_eq!(line_of(contents, "five"), None);
    }

    #[test]
    fn test_parse_link() {
        let root = Path::new("/repo");
        assert_eq!(
            parse_link("https://example.com/a", Some(root)),
            Link::Url("https://example.com/a".into())
        );
        assert_eq!(
            parse_link("src/main.rs:12", Some(root)),
            Link::File {
                path: PathBuf::from("/repo/src/main.rs"),
                line: Some(12)
            }
        );
        assert_eq!(
            parse_link("src/main.rs#L7", Some(root)),
            Link::File {
                path: PathBuf::from("/repo/src/main.rs"),
                line: Some(7)
            }
        );
        assert_eq!(
            parse_link("notes.md", None),
            Link::File {
                path: PathBuf::from("notes.md"),
                line: None
            }
        );
    }

    /// Choosing "Agent Timeline" in a terminal's tab menu dispatches the
    /// action to the terminal view itself, while it's being updated.
    #[gpui::test]
    async fn test_show_agent_timeline_from_the_terminal(cx: &mut gpui::TestAppContext) {
        let (project, workspace, window_handle) = crate::tests::init_test_with_window(cx).await;
        let (terminal_pane, terminal, terminal_view) =
            crate::tests::add_display_only_terminal(&project, window_handle, true, false, cx);
        let cx = &mut gpui::VisualTestContext::from_window(window_handle.into(), cx);
        terminal.update(cx, |terminal, cx| {
            terminal.write_raw_output(
                b"\x1b]133;A\x07$ \x1b]133;B\x07claude\r\n\x1b]633;E;claude\x07\x1b]133;C\x07",
                cx,
            )
        });
        cx.run_until_parked();
        assert!(terminal_view.read_with(cx, |view, cx| view.has_run_agent(cx)));

        let timelines = |cx: &mut gpui::VisualTestContext| {
            workspace.read_with(cx, |workspace, cx| {
                workspace.items_of_type::<AgentTimeline>(cx).count()
            })
        };
        cx.dispatch_action(ShowAgentTimeline);
        cx.run_until_parked();
        assert_eq!(timelines(cx), 1);
        let timeline_pane = workspace.read_with(cx, |workspace, _| workspace.active_pane().clone());
        assert_ne!(timeline_pane, terminal_pane, "opens beside the terminal");

        // Again from the terminal: switches to the open timeline.
        cx.update(|window, cx| {
            let focus_handle = terminal_view.read(cx).focus_handle.clone();
            window.focus(&focus_handle, cx);
        });
        cx.run_until_parked();
        assert_eq!(
            workspace.read_with(cx, |workspace, _| workspace.active_pane().clone()),
            terminal_pane
        );
        cx.dispatch_action(ShowAgentTimeline);
        cx.run_until_parked();
        assert_eq!(timelines(cx), 1);
        assert_eq!(
            workspace.read_with(cx, |workspace, _| workspace.active_pane().clone()),
            timeline_pane
        );
    }
}
