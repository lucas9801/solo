//! SOLO: what AI agents running in terminals record about their sessions,
//! read from the agents' own files rather than from the screen, which shows
//! only part of their output and can't tell what they are doing.
//!
//! An [`AgentAdapter`] knows one agent: how to find the session a terminal
//! runs and how to read the agent's transcript into agent-neutral
//! [`TranscriptEntry`]s: Claude Code ([`ClaudeCode`]) and Codex ([`Codex`]).
//!
//! [`SessionState`] collects the entries of a session: its messages, tool
//! calls, file changes, title and token usage. [`AgentSession`] keeps one up to
//! date as the agent appends to its transcript, reading only what was added,
//! in the background.

mod claude_code;
mod codex;
pub mod codex_otel;
pub mod copilot_otel;
mod live;
pub mod response_timing;

use std::{
    io::{Read as _, Seek as _, SeekFrom},
    path::{Path, PathBuf},
    time::SystemTime,
};

use collections::HashMap;

pub use claude_code::ClaudeCode;
pub use codex::Codex;
pub use live::{AgentSession, AgentSessionEvent};
pub use response_timing::{ResponseSummary, ResponseTiming, ResponseTimings};

/// The agents Solo reads sessions of.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AgentKind {
    ClaudeCode,
    Codex,
}

impl AgentKind {
    pub fn name(self) -> &'static str {
        match self {
            AgentKind::ClaudeCode => "Claude Code",
            AgentKind::Codex => "Codex",
        }
    }

    pub fn adapter(self) -> &'static dyn AgentAdapter {
        match self {
            AgentKind::ClaudeCode => &ClaudeCode,
            AgentKind::Codex => &Codex,
        }
    }
}

/// Every agent adapter.
pub fn adapters() -> [&'static dyn AgentAdapter; 2] {
    [&ClaudeCode, &Codex]
}

/// The adapter for the agent `command` starts, e.g. `claude --resume`.
pub fn adapter_for_command(command: &str) -> Option<&'static dyn AgentAdapter> {
    let program = program_name(command)?;
    adapters()
        .into_iter()
        .find(|adapter| adapter.runs_program(&program))
}

/// The program `command` runs, lowercased and without directory or extension.
fn program_name(command: &str) -> Option<String> {
    let program = command.split_whitespace().next()?.trim_matches(['"', '\'']);
    let name = Path::new(program).file_stem()?.to_string_lossy();
    Some(name.to_lowercase())
}

/// SOLO: a prompt's text with an `[image]` marker for each of its `images`
/// that the text doesn't already name. Agents put an `[Image #1]` placeholder
/// in the text where the user pasted an image; an image sent without one (or
/// with no text at all) still shows.
pub(crate) fn prompt_with_images(text: &str, images: usize) -> String {
    let text = text.trim();
    if images == 0 || text.contains("[Image #") {
        return text.to_string();
    }
    let markers = vec!["[image]"; images].join(" ");
    if text.is_empty() {
        markers
    } else {
        format!("{markers} {text}")
    }
}

/// What is known about a terminal to find the session of the agent it runs.
#[derive(Clone, Debug, Default)]
pub struct TerminalProbe {
    pub working_directory: Option<PathBuf>,
    /// The command that started the agent.
    pub command: Option<String>,
    /// When the agent started.
    pub started_at: Option<SystemTime>,
    /// The process the terminal started (its shell); the agent runs below it.
    pub shell_pid: Option<u32>,
    /// The session id the agent reported itself, through a hook.
    pub reported_session_id: Option<String>,
}

/// How a session was matched to a terminal, from the surest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatchedBy {
    /// The user picked it.
    Chosen,
    /// The agent reported its session id.
    Reported,
    /// The agent's process, running in the terminal, recorded its session id.
    Process,
    /// The session in the terminal's directory written to since the agent
    /// started. `ambiguous` when more than one was.
    Recent { ambiguous: bool },
}

/// A session's transcript, and how it was found.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionLocation {
    pub agent: AgentKind,
    pub session_id: String,
    pub path: PathBuf,
    pub modified: Option<SystemTime>,
    pub matched_by: MatchedBy,
}

/// One AI agent: finding its sessions and reading their transcripts.
///
/// Transcript formats are undocumented and change between versions, so
/// parsing skips whatever it doesn't know instead of failing.
pub trait AgentAdapter: Send + Sync + 'static {
    fn kind(&self) -> AgentKind;

    /// Whether `program` (lowercased, without extension) is this agent.
    fn runs_program(&self, program: &str) -> bool;

    /// The session of the agent running in a terminal, if one can be told.
    /// Reads the file system and the process table: run it in the background.
    fn find_session(&self, probe: &TerminalProbe) -> Option<SessionLocation>;

    /// The sessions recorded for a working directory, newest first, for the
    /// user to pick from when the right one can't be told.
    fn sessions_in(&self, working_directory: &Path) -> Vec<SessionLocation>;

    /// A short description of a session (its title or last prompt), for
    /// picking it. Reads the end of its transcript.
    fn describe_session(&self, location: &SessionLocation) -> Option<String>;

    /// The entries in one line of a transcript; none for lines it doesn't know.
    fn parse_line(&self, line: &str) -> Vec<TranscriptEntry>;

    /// How many tokens of context `model` takes, when known for sure.
    fn context_window(&self, model: &str) -> Option<u64>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

/// A prompt from the user or text the agent wrote.
#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    pub role: Role,
    pub text: String,
    /// The transcript's id for the entry.
    pub id: Option<String>,
    /// The model API's id for the message; an assistant message may come in
    /// several entries that share it.
    pub api_message_id: Option<String>,
    /// As recorded, e.g. `2026-11-01T13:44:36.229Z`.
    pub timestamp: Option<String>,
    /// Written by a sub-agent rather than in the main conversation.
    pub sidechain: bool,
}

/// What a tool call does, across agents' tool names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolKind {
    Read,
    Write,
    Edit,
    /// Runs a command.
    Execute,
    Search,
    Fetch,
    /// Starts a sub-agent.
    Delegate,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolOutcome {
    Succeeded,
    Failed,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolCall {
    pub id: String,
    /// As the agent names it, e.g. `Edit` or `PowerShell`.
    pub name: String,
    pub kind: ToolKind,
    /// The file read, written or edited.
    pub file_path: Option<PathBuf>,
    /// The command run.
    pub command: Option<String>,
    pub description: Option<String>,
    /// The call's input, with long strings (file contents) shortened.
    pub input: serde_json::Value,
    pub timestamp: Option<String>,
    pub sidechain: bool,
    /// Unknown until its result is recorded.
    pub outcome: Option<ToolOutcome>,
}

/// A file a tool call wrote or edited.
#[derive(Clone, Debug, PartialEq)]
pub struct FileChange {
    pub path: PathBuf,
    pub kind: ToolKind,
    pub tool_call_id: String,
}

/// Token counts of one or more model requests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TokenUsage {
    /// Input tokens neither read from nor written to the prompt cache.
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
}

impl TokenUsage {
    /// Every input token the model processed, cached or not.
    pub fn total_input(&self) -> u64 {
        self.input
            .saturating_add(self.cache_read)
            .saturating_add(self.cache_creation)
    }

    pub fn total(&self) -> u64 {
        self.total_input().saturating_add(self.output)
    }

    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }

    /// SOLO: saturating, since the counts come from transcripts (R3 L-A2).
    fn add(&mut self, other: &TokenUsage) {
        self.input = self.input.saturating_add(other.input);
        self.output = self.output.saturating_add(other.output);
        self.cache_read = self.cache_read.saturating_add(other.cache_read);
        self.cache_creation = self.cache_creation.saturating_add(other.cache_creation);
    }

    fn subtract(&mut self, other: &TokenUsage) {
        self.input = self.input.saturating_sub(other.input);
        self.output = self.output.saturating_sub(other.output);
        self.cache_read = self.cache_read.saturating_sub(other.cache_read);
        self.cache_creation = self.cache_creation.saturating_sub(other.cache_creation);
    }
}

/// Usage as the agent itself accounts for it, which can include requests the
/// transcript has no messages for (titles, summaries). Claude Code writes it
/// now and then (its `cost-state` entries), not after every request.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ReportedUsage {
    /// The agent's estimate at API prices; not what a subscription is billed.
    pub estimated_cost_usd: Option<f64>,
    pub models: Vec<(String, TokenUsage)>,
}

/// One thing a transcript records, in an agent-neutral form.
#[derive(Clone, Debug, PartialEq)]
pub enum TranscriptEntry {
    Message(Message),
    ToolCall(ToolCall),
    ToolResult {
        tool_call_id: String,
        outcome: ToolOutcome,
    },
    /// The usage of one model request. An agent may record the same request
    /// more than once (Claude Code does for each of its content blocks).
    Usage {
        api_message_id: String,
        model: Option<String>,
        usage: TokenUsage,
        sidechain: bool,
    },
    Title(String),
    ReportedUsage(ReportedUsage),
    /// The conversation was summarized to free context.
    Compacted {
        tokens_after: Option<u64>,
    },
    /// SOLO: the model, or the context window the agent says it has, when
    /// the agent records them apart from requests (Codex does).
    ModelInfo {
        model: Option<String>,
        context_window: Option<u64>,
    },
    /// SOLO: a model request was sent (Claude Code's `api-request` entries).
    RequestStarted {
        request_id: String,
        at_ms: i64,
    },
    /// SOLO: one content block of a request's response was written.
    ResponseBlock(ResponseBlock),
    /// SOLO: the reasoning effort the main conversation's model runs at
    /// ("medium", "xhigh"), as the agent recorded it.
    Effort(String),
    /// SOLO: a call to Codex's `exec` tool, which runs code that runs
    /// commands and changes files. It shows as a command (its code) until
    /// what it did is recorded ([`TranscriptEntry::ExecStep`]).
    ExecCell(ToolCall),
    /// SOLO: commands run or files changed, recorded apart from the call
    /// that did it: the latest `exec` cell of the turn that started by
    /// `started_at_ms`. The first step of a cell takes its place; the cell's
    /// further steps follow.
    ExecStep {
        calls: Vec<ToolCall>,
        started_at_ms: Option<i64>,
    },
}

/// SOLO: an `exec` cell: its place in [`SessionState::tool_calls`], when it
/// was called and whether a step took its place.
#[derive(Clone, Debug)]
struct ExecCell {
    tool_call_ix: usize,
    called_at_ms: Option<i64>,
    replaced: bool,
}

/// SOLO: one content block of a model response, as Claude Code records it:
/// each block is written once it's generated, with how long it took to
/// stream (for thinking and tool calls).
#[derive(Clone, Debug, PartialEq)]
pub struct ResponseBlock {
    pub request_id: String,
    /// Its place in the response, from 0.
    pub index: u64,
    pub written_at_ms: i64,
    /// How long it streamed; unknown for text.
    pub streamed: Option<std::time::Duration>,
    /// The whole response's output tokens, as recorded with the block.
    pub output_tokens: Option<u64>,
    pub sidechain: bool,
}

/// SOLO: what's known of a request whose response Claude Code records block
/// by block.
#[derive(Clone, Debug, Default)]
struct BlockedRequest {
    started_at_ms: Option<i64>,
    /// When block 0 was written and how long it streamed.
    first_block: Option<(i64, Option<std::time::Duration>)>,
    last_written_ms: Option<i64>,
    output_tokens: Option<u64>,
    sidechain: bool,
}

impl BlockedRequest {
    /// The first token is when block 0 started to stream: written, less its
    /// streaming time. Unknown when it was text, which records no time. The
    /// speed is over the rest of the response, up to its last block.
    fn timing(&self) -> ResponseTiming {
        let Some(started) = self.started_at_ms else {
            return ResponseTiming::default();
        };
        let first_token = self.first_block.and_then(|(written, streamed)| {
            let streamed = i64::try_from(streamed?.as_millis()).ok()?;
            response_timing::millis_between(started, written.checked_sub(streamed)?)
        });
        let generation = first_token.and_then(|first_token| {
            let first_at = started.checked_add(i64::try_from(first_token.as_millis()).ok()?)?;
            response_timing::millis_between(first_at, self.last_written_ms?)
        });
        ResponseTiming {
            first_token,
            output_tokens: self.output_tokens,
            generation,
        }
    }
}

/// Token usage of a session.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UsageSummary {
    /// Every request in the session, sub-agents' included.
    pub session: TokenUsage,
    /// The main conversation's requests since the user's last prompt.
    pub turn: TokenUsage,
    /// Tokens of context the main conversation's last request took.
    pub context_tokens: Option<u64>,
    /// The main conversation's last model.
    pub model: Option<String>,
    /// How many tokens of context the model takes, when known for sure.
    pub context_window: Option<u64>,
}

/// From this much of the context window on, it's nearly full.
pub const CONTEXT_NEARLY_FULL: f64 = 0.8;
/// From this much of the context window on, it's about to run out.
pub const CONTEXT_ALMOST_OUT: f64 = 0.95;

impl UsageSummary {
    /// How much of the context window the last request took, from 0 to 1.
    pub fn context_fraction(&self) -> Option<f64> {
        let window = self.context_window.filter(|window| *window > 0)?;
        Some(self.context_tokens? as f64 / window as f64)
    }

    pub fn context_nearly_full(&self) -> bool {
        self.context_fraction()
            .is_some_and(|fraction| fraction >= CONTEXT_NEARLY_FULL)
    }
}

/// SOLO: where a turn of the main conversation starts, as indices into
/// [`SessionState::messages`] and [`SessionState::tool_calls`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TurnStart {
    pub message_ix: usize,
    pub tool_call_ix: usize,
}

/// What a session's transcript recorded so far.
#[derive(Clone, Debug)]
pub struct SessionState {
    agent: AgentKind,
    /// The user's prompts and the agent's text, in order. The text blocks of
    /// one assistant message are joined.
    pub messages: Vec<Message>,
    pub tool_calls: Vec<ToolCall>,
    pub file_changes: Vec<FileChange>,
    pub title: Option<String>,
    pub usage: UsageSummary,
    pub reported_usage: Option<ReportedUsage>,
    /// Where each of the main conversation's turns starts: the user's prompt
    /// in `messages` and the first tool call made after it.
    pub turn_starts: Vec<TurnStart>,
    tool_call_ixs: HashMap<String, usize>,
    /// Each request's usage and the turn it was in.
    request_usage: HashMap<String, (TokenUsage, usize)>,
    turn: usize,
    /// The context window the agent recorded, for models the adapter doesn't
    /// know.
    recorded_context_window: Option<u64>,
    /// SOLO: how fast the model answered each request, when the transcript
    /// tells (Claude Code's does).
    pub responses: ResponseTimings,
    /// SOLO: the reasoning effort of the main conversation's latest request.
    pub effort: Option<String>,
    blocked_requests: HashMap<String, BlockedRequest>,
    /// SOLO: Codex's `exec` cells, in order.
    exec_cells: Vec<ExecCell>,
}

impl SessionState {
    pub fn new(agent: AgentKind) -> Self {
        Self {
            agent,
            messages: Vec::new(),
            tool_calls: Vec::new(),
            file_changes: Vec::new(),
            title: None,
            usage: UsageSummary::default(),
            reported_usage: None,
            turn_starts: Vec::new(),
            tool_call_ixs: HashMap::default(),
            request_usage: HashMap::default(),
            turn: 0,
            recorded_context_window: None,
            responses: ResponseTimings::default(),
            effort: None,
            blocked_requests: HashMap::default(),
            exec_cells: Vec::new(),
        }
    }

    pub fn agent(&self) -> AgentKind {
        self.agent
    }

    /// Reads a whole transcript. Blocks: run it in the background.
    pub fn load(location: &SessionLocation) -> std::io::Result<Self> {
        let adapter = location.agent.adapter();
        let mut state = Self::new(location.agent);
        let mut reader = TranscriptReader::new(location.path.clone());
        loop {
            let new_lines = reader.read_new_lines()?;
            for line in &new_lines.lines {
                state.apply_all(adapter.parse_line(line));
            }
            if !new_lines.more {
                return Ok(state);
            }
        }
    }

    pub fn apply_all(&mut self, entries: impl IntoIterator<Item = TranscriptEntry>) {
        for entry in entries {
            self.apply(entry);
        }
    }

    pub fn apply(&mut self, entry: TranscriptEntry) {
        match entry {
            TranscriptEntry::Message(message) => {
                // SOLO: Codex records a prompt twice in a row (as the
                // request's input and as an event); the second is the same
                // prompt, not a turn of its own. The same text sent again,
                // for a turn of its own, is one.
                if self.agent == AgentKind::Codex
                    && message.role == Role::User
                    && self.is_repeated_prompt(&message)
                {
                    return;
                }
                // SOLO: Codex records no title: its first prompt is one.
                if self.agent == AgentKind::Codex
                    && self.title.is_none()
                    && message.role == Role::User
                    && !message.sidechain
                {
                    self.title = Some(codex::first_line(&message.text));
                }
                if message.role == Role::User && !message.sidechain {
                    self.turn += 1;
                    self.usage.turn = TokenUsage::default();
                    self.turn_starts.push(TurnStart {
                        message_ix: self.messages.len(),
                        tool_call_ix: self.tool_calls.len(),
                    });
                }
                if let Some(last) = self.messages.last_mut()
                    && message.role == Role::Assistant
                    && last.role == Role::Assistant
                    && message.api_message_id.is_some()
                    && last.api_message_id == message.api_message_id
                {
                    last.text.push_str("\n\n");
                    last.text.push_str(&message.text);
                } else {
                    self.messages.push(message);
                }
            }
            TranscriptEntry::ToolCall(call) => self.add_tool_call(call),
            TranscriptEntry::ExecCell(call) => {
                self.exec_cells.push(ExecCell {
                    tool_call_ix: self.tool_calls.len(),
                    called_at_ms: call
                        .timestamp
                        .as_deref()
                        .and_then(response_timing::parse_timestamp_ms),
                    replaced: false,
                });
                self.add_tool_call(call);
            }
            TranscriptEntry::ExecStep {
                calls,
                started_at_ms,
            } => self.add_exec_step(calls, started_at_ms),
            TranscriptEntry::ToolResult {
                tool_call_id,
                outcome,
            } => {
                if let Some(call) = self
                    .tool_call_ixs
                    .get(&tool_call_id)
                    .and_then(|ix| self.tool_calls.get_mut(*ix))
                {
                    call.outcome = Some(outcome);
                }
            }
            TranscriptEntry::Usage {
                api_message_id,
                model,
                usage,
                sidechain,
            } => self.apply_usage(api_message_id, model, usage, sidechain),
            TranscriptEntry::Title(title) => self.title = Some(title),
            TranscriptEntry::ReportedUsage(reported) => self.reported_usage = Some(reported),
            TranscriptEntry::Compacted { tokens_after } => {
                self.usage.context_tokens = tokens_after;
            }
            TranscriptEntry::ModelInfo {
                model,
                context_window,
            } => {
                if context_window.is_some() {
                    self.recorded_context_window = context_window;
                }
                if let Some(model) = model {
                    self.usage.model = Some(model);
                }
                self.usage.context_window = self
                    .usage
                    .model
                    .as_deref()
                    .and_then(|model| self.agent.adapter().context_window(model))
                    .or(self.recorded_context_window);
            }
            TranscriptEntry::RequestStarted { request_id, at_ms } => {
                let request = self.blocked_requests.entry(request_id.clone()).or_default();
                request.started_at_ms = Some(at_ms);
                let (timing, sidechain) = (request.timing(), request.sidechain);
                self.responses.record(&request_id, timing, sidechain);
            }
            TranscriptEntry::ResponseBlock(block) => {
                let request = self
                    .blocked_requests
                    .entry(block.request_id.clone())
                    .or_default();
                if block.index == 0 {
                    request.first_block = Some((block.written_at_ms, block.streamed));
                }
                request.last_written_ms = Some(
                    request
                        .last_written_ms
                        .map_or(block.written_at_ms, |last| last.max(block.written_at_ms)),
                );
                if block.output_tokens.is_some() {
                    request.output_tokens = request.output_tokens.max(block.output_tokens);
                }
                request.sidechain = block.sidechain;
                if request.started_at_ms.is_some() {
                    let (timing, sidechain) = (request.timing(), request.sidechain);
                    self.responses.record(&block.request_id, timing, sidechain);
                }
            }
            TranscriptEntry::Effort(effort) => self.effort = Some(effort),
        }
    }

    fn add_tool_call(&mut self, call: ToolCall) {
        self.record_file_change(&call);
        self.tool_call_ixs
            .insert(call.id.clone(), self.tool_calls.len());
        self.tool_calls.push(call);
    }

    fn record_file_change(&mut self, call: &ToolCall) {
        if matches!(call.kind, ToolKind::Write | ToolKind::Edit)
            && let Some(path) = &call.file_path
        {
            self.file_changes.push(FileChange {
                path: path.clone(),
                kind: call.kind,
                tool_call_id: call.id.clone(),
            });
        }
    }

    /// SOLO: what an `exec` cell did. Its first step takes the cell's place,
    /// so the cell doesn't show as well; its result no longer applies.
    fn add_exec_step(&mut self, calls: Vec<ToolCall>, started_at_ms: Option<i64>) {
        let turn_start = self
            .turn_starts
            .last()
            .map_or(0, |start| start.tool_call_ix);
        let cell = self
            .exec_cells
            .iter_mut()
            .rev()
            .take_while(|cell| cell.tool_call_ix >= turn_start)
            .find(|cell| match (cell.called_at_ms, started_at_ms) {
                // Codex records the call before running it.
                (Some(called), Some(started)) => called <= started,
                _ => true,
            })
            .filter(|cell| !cell.replaced && !calls.is_empty())
            .map(|cell| {
                cell.replaced = true;
                cell.tool_call_ix
            });
        let mut calls = calls.into_iter();
        if let Some(ix) = cell
            && let Some(first) = calls.next()
        {
            self.record_file_change(&first);
            let first_id = first.id.clone();
            if let Some(slot) = self.tool_calls.get_mut(ix) {
                let cell = std::mem::replace(slot, first);
                self.tool_call_ixs.remove(&cell.id);
                self.tool_call_ixs.insert(first_id, ix);
            }
        }
        for call in calls {
            self.add_tool_call(call);
        }
    }

    /// SOLO: whether `prompt` records the last prompt again, with nothing
    /// recorded since: the same text for the same turn. Codex's prompts carry
    /// their turn's id when the record tells; two without one are two turns
    /// (the user sent the same text again), and one without one is the event
    /// recorded next to the request's input.
    fn is_repeated_prompt(&self, prompt: &Message) -> bool {
        let Some(start) = self.turn_starts.last() else {
            return false;
        };
        let Some(last) = self.messages.get(start.message_ix) else {
            return false;
        };
        let same_turn = match (&last.id, &prompt.id) {
            (Some(last), Some(this)) => last == this,
            (None, None) => false,
            _ => true,
        };
        same_turn
            && start.message_ix + 1 == self.messages.len()
            && start.tool_call_ix == self.tool_calls.len()
            && last.text == prompt.text
            && last.sidechain == prompt.sidechain
    }

    fn apply_usage(
        &mut self,
        api_message_id: String,
        model: Option<String>,
        usage: TokenUsage,
        sidechain: bool,
    ) {
        let previous = self
            .request_usage
            .insert(api_message_id, (usage, self.turn));
        if let Some((previous, turn)) = previous {
            self.usage.session.subtract(&previous);
            if turn == self.turn && !sidechain {
                self.usage.turn.subtract(&previous);
            }
        }
        self.usage.session.add(&usage);
        if sidechain {
            return;
        }
        if previous.is_none_or(|(_, turn)| turn == self.turn) {
            self.usage.turn.add(&usage);
        }
        let context_tokens = usage.total_input();
        self.usage.context_tokens = Some(context_tokens);
        if let Some(model) = model {
            self.usage.context_window = self
                .agent
                .adapter()
                .context_window(&model)
                .or(self.recorded_context_window);
            self.usage.model = Some(model);
        } else if self.usage.context_window.is_none() {
            self.usage.context_window = self.recorded_context_window;
        }
        // A request that took more than the model is known to take ran with a
        // larger window than known: say nothing rather than something wrong.
        if self
            .usage
            .context_window
            .is_some_and(|window| context_tokens > window)
        {
            self.usage.context_window = None;
        }
    }

    /// SOLO: when the user sent the main conversation's first prompt, in
    /// milliseconds since the epoch, as recorded.
    pub fn first_prompt_at_ms(&self) -> Option<i64> {
        let start = self.turn_starts.first()?;
        let message = self.messages.get(start.message_ix)?;
        response_timing::parse_timestamp_ms(message.timestamp.as_deref()?)
    }

    /// The main conversation's last assistant message.
    pub fn last_assistant_message(&self) -> Option<&Message> {
        self.messages
            .iter()
            .rev()
            .find(|message| message.role == Role::Assistant && !message.sidechain)
    }

    /// The main conversation's assistant messages, latest first.
    pub fn assistant_messages_latest_first(&self) -> impl Iterator<Item = &Message> {
        self.messages
            .iter()
            .rev()
            .filter(|message| message.role == Role::Assistant && !message.sidechain)
    }
}

/// Lines read from a transcript.
#[derive(Debug, Default)]
pub struct NewLines {
    /// The file got shorter, or was replaced (SOLO: what was read last isn't
    /// where it was), so it was read again from the start: what was read
    /// before no longer applies.
    pub restarted: bool,
    pub lines: Vec<String>,
    /// SOLO: there's more to read already: a read takes at most
    /// [`MAX_READ_BYTES`] (R3 L-A1).
    pub more: bool,
}

/// SOLO: the most of a transcript read at once, so a transcript of hundreds
/// of MB is read (and applied) a piece at a time rather than held whole, a few
/// times over, in memory (R3 L-A1).
pub const MAX_READ_BYTES: u64 = 16 * 1024 * 1024;

/// Reads what was appended to a transcript since the last read. A line still
/// being written (with no newline yet) waits for the next read.
#[derive(Debug)]
pub struct TranscriptReader {
    path: PathBuf,
    offset: u64,
    partial_line: Vec<u8>,
    /// SOLO: the last bytes read, up to [`TAIL_BYTES`], ending at `offset`: if
    /// the file no longer has them there, it was replaced with another, maybe
    /// longer one (R3 L-A4).
    tail: Vec<u8>,
    max_read_bytes: u64,
}

const TAIL_BYTES: usize = 64;

impl TranscriptReader {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            offset: 0,
            partial_line: Vec::new(),
            tail: Vec::new(),
            max_read_bytes: MAX_READ_BYTES,
        }
    }

    /// SOLO: reads only what's appended from now on (from the start, if the
    /// file isn't there yet).
    pub fn from_end(path: PathBuf) -> Self {
        let offset = std::fs::metadata(&path).map_or(0, |metadata| metadata.len());
        Self {
            path,
            offset,
            partial_line: Vec::new(),
            tail: Vec::new(),
            max_read_bytes: MAX_READ_BYTES,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn read_new_lines(&mut self) -> std::io::Result<NewLines> {
        let mut file = std::fs::File::open(&self.path)?;
        let len = file.metadata()?.len();
        let mut new_lines = NewLines::default();
        if len < self.offset || !self.tail_is_in_place(&mut file)? {
            self.offset = 0;
            self.partial_line.clear();
            self.tail.clear();
            new_lines.restarted = true;
        }
        if len == self.offset {
            return Ok(new_lines);
        }
        file.seek(SeekFrom::Start(self.offset))?;
        let mut bytes = std::mem::take(&mut self.partial_line);
        let partial_len = bytes.len();
        let read = file
            .take((len - self.offset).min(self.max_read_bytes))
            .read_to_end(&mut bytes)?;
        self.offset += read as u64;
        new_lines.more = read > 0 && self.offset < len;
        self.tail.extend_from_slice(&bytes[partial_len..]);
        let extra = self.tail.len().saturating_sub(TAIL_BYTES);
        self.tail.drain(..extra);
        let complete = bytes
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |ix| ix + 1);
        self.partial_line = bytes.split_off(complete);
        new_lines.lines = String::from_utf8_lossy(&bytes)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect();
        Ok(new_lines)
    }

    /// Whether the file still has the bytes last read where they were.
    fn tail_is_in_place(&self, file: &mut std::fs::File) -> std::io::Result<bool> {
        if self.tail.is_empty() {
            return Ok(true);
        }
        let start = self.offset - self.tail.len() as u64;
        file.seek(SeekFrom::Start(start))?;
        let mut bytes = vec![0; self.tail.len()];
        file.read_exact(&mut bytes)?;
        Ok(bytes == self.tail)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    /// Counts come from transcripts, so they can be anything; adding them
    /// doesn't overflow (R3 L-A2).
    #[test]
    fn test_token_counts_saturate() {
        let huge = TokenUsage {
            input: u64::MAX,
            output: u64::MAX,
            cache_read: u64::MAX,
            cache_creation: u64::MAX,
        };
        let mut sum = huge;
        sum.add(&huge);
        assert_eq!(sum, huge);
        assert_eq!(sum.total_input(), u64::MAX);
        assert_eq!(sum.total(), u64::MAX);
    }

    #[test]
    fn test_reader_waits_for_whole_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut file = std::fs::File::create(&path).unwrap();
        let mut reader = TranscriptReader::new(path.clone());
        assert!(reader.read_new_lines().unwrap().lines.is_empty());

        file.write_all(b"{\"a\":1}\n{\"b\":").unwrap();
        file.flush().unwrap();
        assert_eq!(reader.read_new_lines().unwrap().lines, ["{\"a\":1}"]);
        assert!(reader.read_new_lines().unwrap().lines.is_empty());

        file.write_all(b"2}\r\n\n{\"c\":3}\n").unwrap();
        file.flush().unwrap();
        let new_lines = reader.read_new_lines().unwrap();
        assert!(!new_lines.restarted);
        assert_eq!(new_lines.lines, ["{\"b\":2}", "{\"c\":3}"]);

        drop(file);
        std::fs::write(&path, "{\"d\":4}\n").unwrap();
        let new_lines = reader.read_new_lines().unwrap();
        assert!(new_lines.restarted);
        assert_eq!(new_lines.lines, ["{\"d\":4}"]);

        // Replaced with a longer file: read again from the start (R3 L-A4).
        std::fs::write(&path, "{\"e\":5}\n{\"f\":6}\n").unwrap();
        let new_lines = reader.read_new_lines().unwrap();
        assert!(new_lines.restarted);
        assert_eq!(new_lines.lines, ["{\"e\":5}", "{\"f\":6}"]);
        // Appended to: not.
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        file.write_all(b"{\"g\":7}\n").unwrap();
        let new_lines = reader.read_new_lines().unwrap();
        assert!(!new_lines.restarted);
        assert_eq!(new_lines.lines, ["{\"g\":7}"]);
    }

    /// A long transcript is read a piece at a time, each piece whole lines,
    /// with `more` saying there's more (R3 L-A1).
    #[test]
    fn test_reader_reads_long_transcripts_in_pieces() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        std::fs::write(&path, "{\"a\":1}\n{\"b\":2}\n{\"c\":3}\n").unwrap();
        let mut reader = TranscriptReader::new(path);
        reader.max_read_bytes = 10;
        let mut lines = Vec::new();
        let mut reads = 0;
        loop {
            let new_lines = reader.read_new_lines().unwrap();
            reads += 1;
            lines.extend(new_lines.lines);
            if !new_lines.more {
                break;
            }
        }
        assert_eq!(lines, ["{\"a\":1}", "{\"b\":2}", "{\"c\":3}"]);
        assert!(reads >= 3, "{reads}");
    }

    #[test]
    fn test_adapter_for_command() {
        assert_eq!(
            adapter_for_command("claude --resume").map(|adapter| adapter.kind()),
            Some(AgentKind::ClaudeCode)
        );
        assert_eq!(
            adapter_for_command(r#""C:\Users\me\.local\bin\claude.exe""#)
                .map(|adapter| adapter.kind()),
            Some(AgentKind::ClaudeCode)
        );
        assert!(adapter_for_command("cargo test").is_none());
        assert!(adapter_for_command("").is_none());
    }
}
