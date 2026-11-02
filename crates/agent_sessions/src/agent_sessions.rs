//! SOLO: what AI agents running in terminals record about their sessions,
//! read from the agents' own files rather than from the screen, which shows
//! only part of their output and can't tell what they are doing.
//!
//! An [`AgentAdapter`] knows one agent: how to find the session a terminal
//! runs and how to read the agent's transcript into agent-neutral
//! [`TranscriptEntry`]s. Claude Code ([`ClaudeCode`]) is the only one so far;
//! Codex and Gemini CLI would be further adapters.
//!
//! [`SessionState`] collects the entries of a session: its messages, tool
//! calls, file changes, title and token usage. [`AgentSession`] keeps one up to
//! date as the agent appends to its transcript, reading only what was added,
//! in the background.

mod claude_code;
mod live;

use std::{
    io::{Read as _, Seek as _, SeekFrom},
    path::{Path, PathBuf},
    time::SystemTime,
};

use collections::HashMap;

pub use claude_code::ClaudeCode;
pub use live::{AgentSession, AgentSessionEvent};

/// The agents Solo reads sessions of.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AgentKind {
    ClaudeCode,
}

impl AgentKind {
    pub fn name(self) -> &'static str {
        match self {
            AgentKind::ClaudeCode => "Claude Code",
        }
    }

    pub fn adapter(self) -> &'static dyn AgentAdapter {
        match self {
            AgentKind::ClaudeCode => &ClaudeCode,
        }
    }
}

/// Every agent adapter.
pub fn adapters() -> [&'static dyn AgentAdapter; 1] {
    [&ClaudeCode]
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
        self.input + self.cache_read + self.cache_creation
    }

    pub fn total(&self) -> u64 {
        self.total_input() + self.output
    }

    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }

    fn add(&mut self, other: &TokenUsage) {
        self.input += other.input;
        self.output += other.output;
        self.cache_read += other.cache_read;
        self.cache_creation += other.cache_creation;
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
        for line in reader.read_new_lines()?.lines {
            state.apply_all(adapter.parse_line(&line));
        }
        Ok(state)
    }

    pub fn apply_all(&mut self, entries: impl IntoIterator<Item = TranscriptEntry>) {
        for entry in entries {
            self.apply(entry);
        }
    }

    pub fn apply(&mut self, entry: TranscriptEntry) {
        match entry {
            TranscriptEntry::Message(message) => {
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
            TranscriptEntry::ToolCall(call) => {
                if matches!(call.kind, ToolKind::Write | ToolKind::Edit)
                    && let Some(path) = &call.file_path
                {
                    self.file_changes.push(FileChange {
                        path: path.clone(),
                        kind: call.kind,
                        tool_call_id: call.id.clone(),
                    });
                }
                self.tool_call_ixs
                    .insert(call.id.clone(), self.tool_calls.len());
                self.tool_calls.push(call);
            }
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
        }
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
            self.usage.context_window = self.agent.adapter().context_window(&model);
            self.usage.model = Some(model);
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
    /// The file got shorter, so it was read again from the start: what was
    /// read before no longer applies.
    pub restarted: bool,
    pub lines: Vec<String>,
}

/// Reads what was appended to a transcript since the last read. A line still
/// being written (with no newline yet) waits for the next read.
#[derive(Debug)]
pub struct TranscriptReader {
    path: PathBuf,
    offset: u64,
    partial_line: Vec<u8>,
}

impl TranscriptReader {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            offset: 0,
            partial_line: Vec::new(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn read_new_lines(&mut self) -> std::io::Result<NewLines> {
        let mut file = std::fs::File::open(&self.path)?;
        let len = file.metadata()?.len();
        let mut new_lines = NewLines::default();
        if len < self.offset {
            self.offset = 0;
            self.partial_line.clear();
            new_lines.restarted = true;
        }
        if len == self.offset {
            return Ok(new_lines);
        }
        file.seek(SeekFrom::Start(self.offset))?;
        let mut bytes = std::mem::take(&mut self.partial_line);
        let read = file.take(len - self.offset).read_to_end(&mut bytes)?;
        self.offset += read as u64;
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

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
