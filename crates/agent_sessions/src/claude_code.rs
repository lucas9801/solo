//! Claude Code's sessions: where they are and how to read them. Everything
//! about its file formats is here.
//!
//! Each session's transcript is `~/.claude/projects/<dir>/<session id>.jsonl`
//! (`CLAUDE_CONFIG_DIR` replaces `~/.claude`), one JSON entry per line, only
//! ever appended to. `<dir>` is the working directory Claude started in with
//! every character but ASCII letters and digits turned into `-`:
//! `D:\GodotEngine` is `D--GodotEngine`, `G:\Work\godot_engine` is
//! `G--Work-godot-engine`. Seen, not documented.
//!
//! Entries have a `type`. The ones read here:
//! - `user`: a prompt (`message.content` a string or text blocks), or tool
//!   results (`tool_result` blocks). `isMeta` and `isCompactSummary` entries
//!   and text in tags (`<command-name>`, `<task-notification>`) aren't
//!   prompts the user typed.
//! - `assistant`: one content block of a model response in `message.content`
//!   (`text`, `thinking` or `tool_use`); a response's blocks come in separate
//!   entries sharing `message.id`, each with the response's `message.usage`.
//! - `ai-title`: the session title, `aiTitle`.
//! - `cost-state`: Claude Code's own running totals, with `modelUsage` per
//!   model and `totalCostUSD`, its estimate at API prices. Written now and then
//!   (not by every version), and counting requests with no transcript entries.
//! - `system` with `subtype` `compact_boundary`: the conversation was
//!   compacted; `compactMetadata.postTokens` is what's left.
//!
//! Others (`attachment`, `file-history-snapshot`, `last-prompt`, `mode`, ...)
//! are skipped, as are fields not named here.
//!
//! Claude Code also records each running instance in `~/.claude/sessions/<pid>.json`,
//! with its `sessionId` and `cwd`, which tells which session a process runs.

use std::{
    io::{Read as _, Seek as _, SeekFrom},
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use serde_json::Value;

use crate::{
    AgentAdapter, AgentKind, MatchedBy, Message, ReportedUsage, Role, SessionLocation,
    TerminalProbe, TokenUsage, ToolCall, ToolKind, ToolOutcome, TranscriptEntry,
};

/// Strings in tool inputs longer than this (file contents) are cut short.
const MAX_INPUT_STRING_CHARS: usize = 1000;
/// Long project directory names are cut to this and a hash added.
const MAX_PROJECT_DIR_CHARS: usize = 200;
/// A transcript written to this long before the agent started (the clocks
/// of file times and process start times differ a little) still counts.
const START_SLACK: Duration = Duration::from_secs(5);
/// How much of the end of a transcript is read to describe it.
const DESCRIBE_TAIL_BYTES: u64 = 256 * 1024;

pub struct ClaudeCode;

impl AgentAdapter for ClaudeCode {
    fn kind(&self) -> AgentKind {
        AgentKind::ClaudeCode
    }

    fn runs_program(&self, program: &str) -> bool {
        program == "claude"
    }

    fn find_session(&self, probe: &TerminalProbe) -> Option<SessionLocation> {
        let config_dir = config_dir()?;
        find_session_in(&config_dir, probe)
    }

    fn sessions_in(&self, working_directory: &Path) -> Vec<SessionLocation> {
        let Some(config_dir) = config_dir() else {
            return Vec::new();
        };
        let Some(dir) = project_dir(&config_dir.join("projects"), working_directory) else {
            return Vec::new();
        };
        transcripts_in(&dir)
    }

    fn describe_session(&self, location: &SessionLocation) -> Option<String> {
        describe_transcript(&location.path)
    }

    fn parse_line(&self, line: &str) -> Vec<TranscriptEntry> {
        parse_line(line)
    }

    fn context_window(&self, model: &str) -> Option<u64> {
        context_window(model)
    }
}

fn config_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|dir| !dir.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    let dir = util::paths::home_dir().join(".claude");
    dir.is_dir().then_some(dir)
}

fn find_session_in(config_dir: &Path, probe: &TerminalProbe) -> Option<SessionLocation> {
    let projects = config_dir.join("projects");
    let working_directory = probe.working_directory.as_deref().map(native_path);
    let located = |session_id: &str, matched_by| {
        let path = transcript_path(&projects, working_directory.as_deref(), session_id)?;
        Some(SessionLocation {
            agent: AgentKind::ClaudeCode,
            session_id: session_id.to_string(),
            modified: modified(&path),
            path,
            matched_by,
        })
    };
    if let Some(session_id) = &probe.reported_session_id
        && let Some(location) = located(session_id, MatchedBy::Reported)
    {
        return Some(location);
    }
    if let Some(shell_pid) = probe.shell_pid
        && let Some(session_id) = session_of_process_below(config_dir, shell_pid)
        && let Some(location) = located(&session_id, MatchedBy::Process)
    {
        return Some(location);
    }
    let dir = project_dir(&projects, working_directory.as_deref()?)?;
    let transcripts = transcripts_in(&dir);
    let Some(started_at) = probe.started_at else {
        let mut newest = transcripts.into_iter().next()?;
        newest.matched_by = MatchedBy::Recent { ambiguous: true };
        return Some(newest);
    };
    let since = started_at.checked_sub(START_SLACK).unwrap_or(started_at);
    let mut written_since = transcripts
        .into_iter()
        .filter(|location| location.modified.is_some_and(|modified| modified >= since));
    // Nothing yet: Claude writes its transcript once the first prompt is sent.
    let mut newest = written_since.next()?;
    newest.matched_by = MatchedBy::Recent {
        ambiguous: written_since.next().is_some(),
    };
    Some(newest)
}

/// A path as Claude, a native program, sees it: Git Bash and MSYS report
/// `/d/work` for `D:\work`.
fn native_path(path: &Path) -> PathBuf {
    if cfg!(windows)
        && let Some(path) = path.to_str()
        && let Some(rest) = path.strip_prefix('/')
    {
        let mut chars = rest.chars();
        if let Some(drive) = chars.next().filter(char::is_ascii_alphabetic)
            && matches!(chars.next(), None | Some('/'))
        {
            let rest = chars.as_str().trim_start_matches('/').replace('/', "\\");
            return PathBuf::from(format!("{}:\\{rest}", drive.to_ascii_uppercase()));
        }
    }
    path.to_path_buf()
}

/// The name of the projects directory for a working directory.
pub(crate) fn project_dir_name(working_directory: &Path) -> String {
    working_directory
        .to_string_lossy()
        .chars()
        .flat_map(|char| {
            let replaced = if char.is_ascii_alphanumeric() {
                char
            } else {
                '-'
            };
            // Claude replaces UTF-16 code units, so a character outside the
            // Basic Multilingual Plane becomes two.
            std::iter::repeat_n(replaced, if replaced == '-' { char.len_utf16() } else { 1 })
        })
        .collect()
}

/// The projects directory for a working directory. Drive letters may differ
/// in case, and long names end in a hash that can't be worked out here.
fn project_dir(projects: &Path, working_directory: &Path) -> Option<PathBuf> {
    let name = project_dir_name(working_directory);
    let exact = projects.join(&name);
    if exact.is_dir() {
        return Some(exact);
    }
    let prefix: String = name.chars().take(MAX_PROJECT_DIR_CHARS).collect();
    let is_long = name.len() > MAX_PROJECT_DIR_CHARS;
    std::fs::read_dir(projects)
        .ok()?
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .find(|entry| {
            let dir_name = entry.file_name().to_string_lossy().into_owned();
            if is_long {
                dir_name.starts_with(&prefix)
            } else {
                cfg!(windows) && dir_name.eq_ignore_ascii_case(&name)
            }
        })
        .map(|entry| entry.path())
}

/// The transcript of a session: in its working directory's projects
/// directory, or any other (it may have been started elsewhere and resumed).
fn transcript_path(
    projects: &Path,
    working_directory: Option<&Path>,
    session_id: &str,
) -> Option<PathBuf> {
    if !is_session_id(session_id) {
        return None;
    }
    let file_name = format!("{session_id}.jsonl");
    if let Some(dir) = working_directory.and_then(|directory| project_dir(projects, directory)) {
        let path = dir.join(&file_name);
        if path.is_file() {
            return Some(path);
        }
    }
    std::fs::read_dir(projects)
        .ok()?
        .flatten()
        .map(|entry| entry.path().join(&file_name))
        .find(|path| path.is_file())
}

/// Session ids name files, so only what they're made of is accepted.
fn is_session_id(session_id: &str) -> bool {
    !session_id.is_empty()
        && session_id
            .chars()
            .all(|char| char.is_ascii_alphanumeric() || char == '-' || char == '_')
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
}

/// The transcripts in a projects directory, newest first.
fn transcripts_in(dir: &Path) -> Vec<SessionLocation> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut transcripts: Vec<SessionLocation> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension()? != "jsonl" {
                return None;
            }
            let session_id = path.file_stem()?.to_string_lossy().into_owned();
            Some(SessionLocation {
                agent: AgentKind::ClaudeCode,
                session_id,
                modified: entry
                    .metadata()
                    .and_then(|metadata| metadata.modified())
                    .ok(),
                path,
                matched_by: MatchedBy::Recent { ambiguous: true },
            })
        })
        .collect();
    transcripts.sort_by_key(|transcript| std::cmp::Reverse(transcript.modified));
    transcripts
}

/// The session a Claude process below `shell_pid` runs, from the record of
/// running instances.
fn session_of_process_below(config_dir: &Path, shell_pid: u32) -> Option<String> {
    let records: Vec<(u32, String)> = std::fs::read_dir(config_dir.join("sessions"))
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension()? != "json" {
                return None;
            }
            let pid = path.file_stem()?.to_str()?.parse::<u32>().ok()?;
            let record: Value = serde_json::from_str(&std::fs::read_to_string(&path).ok()?).ok()?;
            let session_id = record.get("sessionId")?.as_str()?.to_string();
            Some((pid, session_id))
        })
        .collect();
    if records.is_empty() {
        return None;
    }
    let mut system = sysinfo::System::new();
    system.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::All,
        true,
        sysinfo::ProcessRefreshKind::nothing(),
    );
    let shell_pid = sysinfo::Pid::from_u32(shell_pid);
    records.into_iter().find_map(|(pid, session_id)| {
        let mut current = system.process(sysinfo::Pid::from_u32(pid))?;
        // Bounded, in case parent links loop through reused pids.
        for _ in 0..64 {
            let parent = current.parent()?;
            if parent == shell_pid {
                return Some(session_id);
            }
            current = system.process(parent)?;
        }
        None
    })
}

/// The session's title, or else its last prompt, from the end of its
/// transcript.
fn describe_transcript(path: &Path) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(DESCRIBE_TAIL_BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let mut last_prompt = None;
    for line in text.lines().rev() {
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match entry.get("type").and_then(Value::as_str) {
            Some("ai-title") => {
                if let Some(title) = entry.get("aiTitle").and_then(Value::as_str) {
                    return Some(title.to_string());
                }
            }
            Some("last-prompt") if last_prompt.is_none() => {
                last_prompt = entry
                    .get("lastPrompt")
                    .and_then(Value::as_str)
                    .map(str::to_string);
            }
            _ => {}
        }
    }
    last_prompt
}

pub(crate) fn parse_line(line: &str) -> Vec<TranscriptEntry> {
    let entry = match serde_json::from_str::<Value>(line) {
        Ok(entry) => entry,
        Err(error) => {
            log::debug!("skipping a Claude Code transcript line that isn't JSON: {error}");
            return Vec::new();
        }
    };
    let field = |name: &str| entry.get(name).and_then(Value::as_str);
    let flag = |name: &str| entry.get(name).and_then(Value::as_bool).unwrap_or(false);
    let sidechain = flag("isSidechain");
    match field("type") {
        Some("user") if !flag("isMeta") && !flag("isCompactSummary") => {
            parse_user(&entry, sidechain)
        }
        Some("assistant") => parse_assistant(&entry, sidechain),
        Some("ai-title") => field("aiTitle")
            .filter(|title| !title.trim().is_empty())
            .map(|title| vec![TranscriptEntry::Title(title.trim().to_string())])
            .unwrap_or_default(),
        Some("cost-state") => vec![TranscriptEntry::ReportedUsage(parse_cost_state(&entry))],
        Some("system") if field("subtype") == Some("compact_boundary") => {
            let tokens_after = entry
                .pointer("/compactMetadata/postTokens")
                .and_then(Value::as_u64);
            vec![TranscriptEntry::Compacted { tokens_after }]
        }
        _ => Vec::new(),
    }
}

fn string_field(value: &Value, name: &str) -> Option<String> {
    value.get(name).and_then(Value::as_str).map(str::to_string)
}

/// Text the user typed, rather than something Claude Code put in their name.
fn is_typed_prompt(text: &str) -> bool {
    let text = text.trim_start();
    !text.is_empty() && !text.starts_with('<') && !text.starts_with("[Request interrupted")
}

fn parse_user(entry: &Value, sidechain: bool) -> Vec<TranscriptEntry> {
    let mut entries = Vec::new();
    let mut prompt = Vec::new();
    match entry.pointer("/message/content") {
        Some(Value::String(text)) => prompt.push(text.as_str()),
        Some(Value::Array(blocks)) => {
            for block in blocks {
                match block.get("type").and_then(Value::as_str) {
                    Some("text") => prompt.extend(block.get("text").and_then(Value::as_str)),
                    Some("tool_result") => {
                        let Some(tool_call_id) = string_field(block, "tool_use_id") else {
                            continue;
                        };
                        let failed = block
                            .get("is_error")
                            .and_then(Value::as_bool)
                            .unwrap_or(false);
                        entries.push(TranscriptEntry::ToolResult {
                            tool_call_id,
                            outcome: if failed {
                                ToolOutcome::Failed
                            } else {
                                ToolOutcome::Succeeded
                            },
                        });
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
    let prompt = prompt.join("\n");
    if is_typed_prompt(&prompt) {
        entries.insert(
            0,
            TranscriptEntry::Message(Message {
                role: Role::User,
                text: prompt,
                id: string_field(entry, "uuid"),
                api_message_id: None,
                timestamp: string_field(entry, "timestamp"),
                sidechain,
            }),
        );
    }
    entries
}

fn parse_assistant(entry: &Value, sidechain: bool) -> Vec<TranscriptEntry> {
    let Some(message) = entry.get("message") else {
        return Vec::new();
    };
    let api_message_id = string_field(message, "id");
    let model = string_field(message, "model").filter(|model| !model.starts_with('<'));
    let timestamp = string_field(entry, "timestamp");
    let mut entries = Vec::new();
    for block in message
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                let Some(text) = block.get("text").and_then(Value::as_str) else {
                    continue;
                };
                if text.trim().is_empty() {
                    continue;
                }
                entries.push(TranscriptEntry::Message(Message {
                    role: Role::Assistant,
                    text: text.to_string(),
                    id: string_field(entry, "uuid"),
                    api_message_id: api_message_id.clone(),
                    timestamp: timestamp.clone(),
                    sidechain,
                }));
            }
            Some("tool_use") => {
                if let Some(call) = parse_tool_use(block, timestamp.clone(), sidechain) {
                    entries.push(TranscriptEntry::ToolCall(call));
                }
            }
            _ => {}
        }
    }
    // Claude Code's own error messages have no model and no real usage.
    if let (Some(api_message_id), Some(usage)) = (api_message_id, message.get("usage"))
        && model.is_some()
    {
        let count = |name: &str| usage.get(name).and_then(Value::as_u64).unwrap_or(0);
        let mut model = model;
        // `/model sonnet[1m]` and the like ask for a larger window.
        if let Some(requested) = entry.get("requestedModel").and_then(Value::as_str)
            && requested.ends_with("[1m]")
            && let Some(model) = model.as_mut()
            && !model.ends_with("[1m]")
        {
            model.push_str("[1m]");
        }
        entries.push(TranscriptEntry::Usage {
            api_message_id,
            model,
            usage: TokenUsage {
                input: count("input_tokens"),
                output: count("output_tokens"),
                cache_read: count("cache_read_input_tokens"),
                cache_creation: count("cache_creation_input_tokens"),
            },
            sidechain,
        });
    }
    entries
}

fn parse_tool_use(block: &Value, timestamp: Option<String>, sidechain: bool) -> Option<ToolCall> {
    let id = string_field(block, "id")?;
    let name = string_field(block, "name")?;
    let input = block.get("input").cloned().unwrap_or(Value::Null);
    let kind = tool_kind(&name);
    let input_string = |names: &[&str]| {
        names
            .iter()
            .find_map(|name| input.get(*name).and_then(Value::as_str))
            .map(str::to_string)
    };
    let file_path = match kind {
        ToolKind::Read | ToolKind::Write | ToolKind::Edit => {
            input_string(&["file_path", "notebook_path"]).map(PathBuf::from)
        }
        _ => None,
    };
    Some(ToolCall {
        id,
        kind,
        file_path,
        command: input_string(&["command"]),
        description: input_string(&["description"]),
        input: shortened(input),
        name,
        timestamp,
        sidechain,
        outcome: None,
    })
}

fn tool_kind(name: &str) -> ToolKind {
    match name {
        "Read" | "NotebookRead" => ToolKind::Read,
        "Write" => ToolKind::Write,
        "Edit" | "MultiEdit" | "NotebookEdit" => ToolKind::Edit,
        "Bash" | "PowerShell" | "BashOutput" | "KillShell" => ToolKind::Execute,
        "Grep" | "Glob" | "LS" => ToolKind::Search,
        "WebFetch" | "WebSearch" => ToolKind::Fetch,
        "Task" | "Agent" => ToolKind::Delegate,
        _ => ToolKind::Other,
    }
}

/// `value` with long strings cut short.
fn shortened(value: Value) -> Value {
    match value {
        Value::String(text) if text.chars().count() > MAX_INPUT_STRING_CHARS => {
            let mut text: String = text.chars().take(MAX_INPUT_STRING_CHARS).collect();
            text.push('…');
            Value::String(text)
        }
        Value::Array(values) => Value::Array(values.into_iter().map(shortened).collect()),
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, value)| (key, shortened(value)))
                .collect(),
        ),
        value => value,
    }
}

fn parse_cost_state(entry: &Value) -> ReportedUsage {
    let models = entry
        .get("modelUsage")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .map(|(model, usage)| {
            let count = |name: &str| usage.get(name).and_then(Value::as_u64).unwrap_or(0);
            (
                model.clone(),
                TokenUsage {
                    input: count("inputTokens"),
                    output: count("outputTokens"),
                    cache_read: count("cacheReadInputTokens"),
                    cache_creation: count("cacheCreationInputTokens"),
                },
            )
        })
        .collect();
    ReportedUsage {
        estimated_cost_usd: entry.get("totalCostUSD").and_then(Value::as_f64),
        models,
    }
}

/// Context windows of the models Claude Code runs, as Anthropic documents
/// them. A model not listed has none, rather than a guess.
fn context_window(model: &str) -> Option<u64> {
    const MILLION: u64 = 1_000_000;
    if model.ends_with("[1m]") {
        return Some(MILLION);
    }
    // Dated ids, e.g. `claude-sonnet-4-5-20250929`.
    let model = match model.rsplit_once('-') {
        Some((base, date)) if date.len() == 8 && date.chars().all(|c| c.is_ascii_digit()) => base,
        _ => model,
    };
    match model {
        "claude-fable-5-1" | "claude-mythos-5-1" | "claude-fable-5" | "claude-opus-5-5"
        | "claude-opus-5" | "claude-opus-4-8" | "claude-opus-4-7" | "claude-opus-4-6"
        | "claude-sonnet-5-5" | "claude-sonnet-5" | "claude-sonnet-4-6" | "claude-haiku-5-5" => {
            Some(MILLION)
        }
        "claude-haiku-4-5" | "claude-sonnet-4-5" | "claude-opus-4-5" | "claude-opus-4-1"
        | "claude-opus-4" | "claude-sonnet-4" => Some(200_000),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SessionState, UsageSummary};

    /// A made-up session in the shape of a real Claude Code transcript.
    const SESSION: &str = r#"{"type":"mode","mode":"normal","sessionId":"s1"}
{"type":"file-history-snapshot","messageId":"u1","snapshot":{"messageId":"u1","trackedFileBackups":{},"timestamp":"2026-11-01T13:44:31.829Z"},"isSnapshotUpdate":false}
{"parentUuid":null,"isSidechain":false,"type":"user","message":{"role":"user","content":"Explain the build"},"uuid":"u1","timestamp":"2026-11-01T13:44:31.800Z","cwd":"D:\\Project","sessionId":"s1","version":"2.1.314"}
{"type":"attachment","attachment":{"type":"skill_listing"},"uuid":"a0","sessionId":"s1"}
{"parentUuid":"u1","isSidechain":false,"type":"assistant","message":{"model":"claude-sonnet-5-5","id":"msg_1","type":"message","role":"assistant","content":[{"type":"thinking","thinking":"","signature":"x"}],"stop_reason":"tool_use","usage":{"input_tokens":2,"cache_creation_input_tokens":13673,"cache_read_input_tokens":32912,"output_tokens":305,"service_tier":"standard"}},"requestedModel":"claude-sonnet-5-5","uuid":"a1","timestamp":"2026-11-01T13:44:36.229Z"}
{"parentUuid":"a1","isSidechain":false,"type":"assistant","message":{"model":"claude-sonnet-5-5","id":"msg_1","type":"message","role":"assistant","content":[{"type":"text","text":"Let me look."}],"stop_reason":"tool_use","usage":{"input_tokens":2,"cache_creation_input_tokens":13673,"cache_read_input_tokens":32912,"output_tokens":305}},"uuid":"a2","timestamp":"2026-11-01T13:44:36.231Z"}
{"parentUuid":"a2","isSidechain":false,"type":"assistant","message":{"model":"claude-sonnet-5-5","id":"msg_1","type":"message","role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Read","input":{"file_path":"D:\\Project\\build.rs"}},{"type":"tool_use","id":"toolu_2","name":"PowerShell","input":{"command":"cargo build","description":"Build it"}}],"stop_reason":"tool_use","usage":{"input_tokens":2,"cache_creation_input_tokens":13673,"cache_read_input_tokens":32912,"output_tokens":305}},"uuid":"a3","timestamp":"2026-11-01T13:44:37.472Z"}
{"parentUuid":"a3","isSidechain":false,"type":"user","message":{"role":"user","content":[{"tool_use_id":"toolu_1","type":"tool_result","content":"fn main() {}"},{"tool_use_id":"toolu_2","type":"tool_result","content":"error","is_error":true}]},"uuid":"u2","timestamp":"2026-11-01T13:44:40.000Z"}
{"type":"future-record","whatever":{"nested":[1,2,3]}}
{"parentUuid":"u2","isSidechain":false,"type":"assistant","message":{"model":"claude-sonnet-5-5","id":"msg_2","type":"message","role":"assistant","content":[{"type":"tool_use","id":"toolu_3","name":"Edit","input":{"file_path":"D:\\Project\\build.rs","old_string":"a","new_string":"b"}}],"usage":{"input_tokens":4,"cache_creation_input_tokens":100,"cache_read_input_tokens":46585,"output_tokens":50}},"uuid":"a4","timestamp":"2026-11-01T13:44:45.000Z"}
{"parentUuid":"a4","isSidechain":false,"type":"user","message":{"role":"user","content":[{"tool_use_id":"toolu_3","type":"tool_result","content":"ok"}]},"uuid":"u3","timestamp":"2026-11-01T13:44:46.000Z"}
{"parentUuid":"u3","isSidechain":false,"type":"assistant","message":{"model":"claude-sonnet-5-5","id":"msg_3","type":"message","role":"assistant","content":[{"type":"text","text":"Here it is:\n\n```mermaid\ngraph TD\n  A --> B\n```"}],"stop_reason":"end_turn","usage":{"input_tokens":2,"cache_creation_input_tokens":1643,"cache_read_input_tokens":46685,"output_tokens":3653}},"uuid":"a5","timestamp":"2026-11-01T13:45:10.457Z"}
{"parentUuid":"a5","isSidechain":false,"type":"system","subtype":"turn_duration","durationMs":38736,"messageCount":19,"uuid":"s1x"}
{"type":"ai-title","aiTitle":"Build explained","sessionId":"s1"}
{"type":"cost-state","sessionId":"s1","totalCostUSD":0.1295745,"totalLinesAdded":0,"modelUsage":{"claude-haiku-5-5":{"inputTokens":1180,"outputTokens":16,"cacheReadInputTokens":0,"cacheCreationInputTokens":0,"costUSD":0.000126},"claude-sonnet-5-5":{"inputTokens":510,"outputTokens":3978,"cacheReadInputTokens":127725,"cacheCreationInputTokens":18969,"costUSD":0.1294485}}}
{"parentUuid":"s1x","isSidechain":false,"type":"user","message":{"role":"user","content":"<command-name>/status</command-name>"},"uuid":"u4","timestamp":"2026-11-01T14:00:00.000Z"}
{"parentUuid":"u4","isSidechain":false,"type":"user","message":{"role":"user","content":"Caveat: injected"},"isMeta":true,"uuid":"u5"}
{"parentUuid":"u5","isSidechain":false,"type":"user","message":{"role":"user","content":[{"type":"text","text":"And the tests?"}]},"uuid":"u6","timestamp":"2026-11-01T14:04:04.000Z"}
{"parentUuid":"u6","isSidechain":false,"type":"assistant","message":{"model":"claude-sonnet-5-5","id":"msg_4","type":"message","role":"assistant","content":[{"type":"text","text":"Run cargo test"#;

    fn session() -> SessionState {
        let mut state = SessionState::new(AgentKind::ClaudeCode);
        for line in SESSION.lines() {
            state.apply_all(parse_line(line));
        }
        state
    }

    #[test]
    fn test_messages() {
        let state = session();
        let messages: Vec<(Role, &str)> = state
            .messages
            .iter()
            .map(|message| (message.role, message.text.as_str()))
            .collect();
        assert_eq!(
            messages,
            [
                (Role::User, "Explain the build"),
                (Role::Assistant, "Let me look."),
                (
                    Role::Assistant,
                    "Here it is:\n\n```mermaid\ngraph TD\n  A --> B\n```"
                ),
                (Role::User, "And the tests?"),
            ]
        );
        // The last line was cut off while being written.
        assert_eq!(
            state
                .last_assistant_message()
                .map(|message| message.text.as_str()),
            Some("Here it is:\n\n```mermaid\ngraph TD\n  A --> B\n```")
        );
        assert_eq!(state.title.as_deref(), Some("Build explained"));
    }

    #[test]
    fn test_text_blocks_of_one_response_are_joined() {
        let mut state = SessionState::new(AgentKind::ClaudeCode);
        for text in ["First", "Second"] {
            state.apply_all(parse_line(&format!(
                r#"{{"type":"assistant","message":{{"model":"claude-opus-5-5","id":"msg_9","content":[{{"type":"text","text":"{text}"}}]}}}}"#
            )));
        }
        assert_eq!(state.messages.len(), 1);
        assert_eq!(state.messages[0].text, "First\n\nSecond");
    }

    #[test]
    fn test_tool_calls() {
        let state = session();
        let calls: Vec<(&str, ToolKind, Option<ToolOutcome>)> = state
            .tool_calls
            .iter()
            .map(|call| (call.name.as_str(), call.kind, call.outcome))
            .collect();
        assert_eq!(
            calls,
            [
                ("Read", ToolKind::Read, Some(ToolOutcome::Succeeded)),
                ("PowerShell", ToolKind::Execute, Some(ToolOutcome::Failed)),
                ("Edit", ToolKind::Edit, Some(ToolOutcome::Succeeded)),
            ]
        );
        assert_eq!(
            state.tool_calls[0].file_path.as_deref(),
            Some(Path::new("D:\\Project\\build.rs"))
        );
        assert_eq!(state.tool_calls[1].command.as_deref(), Some("cargo build"));
        assert_eq!(state.tool_calls[1].description.as_deref(), Some("Build it"));
        assert_eq!(state.file_changes.len(), 1);
        assert_eq!(state.file_changes[0].tool_call_id, "toolu_3");
        assert_eq!(state.file_changes[0].kind, ToolKind::Edit);
    }

    #[test]
    fn test_long_tool_input_is_shortened() {
        let content = "x".repeat(5000);
        let line = format!(
            r#"{{"type":"assistant","message":{{"model":"claude-opus-5-5","id":"m","content":[{{"type":"tool_use","id":"t","name":"Write","input":{{"file_path":"/a.txt","content":"{content}"}}}}]}}}}"#
        );
        let entries = parse_line(&line);
        let [TranscriptEntry::ToolCall(call)] = entries.as_slice() else {
            panic!("expected one tool call, got {entries:?}");
        };
        let content = call.input["content"].as_str().unwrap();
        assert_eq!(content.chars().count(), MAX_INPUT_STRING_CHARS + 1);
        assert_eq!(call.file_path.as_deref(), Some(Path::new("/a.txt")));
    }

    #[test]
    fn test_usage() {
        let state = session();
        // Each response counted once, however many entries it came in.
        assert_eq!(
            state.usage.session,
            TokenUsage {
                input: 2 + 4 + 2,
                output: 305 + 50 + 3653,
                cache_read: 32912 + 46585 + 46685,
                cache_creation: 13673 + 100 + 1643,
            }
        );
        // A new prompt started a turn with nothing in it yet.
        assert_eq!(state.usage.turn, TokenUsage::default());
        assert_eq!(state.usage.model.as_deref(), Some("claude-sonnet-5-5"));
        assert_eq!(state.usage.context_tokens, Some(2 + 46685 + 1643));
        assert_eq!(state.usage.context_window, Some(1_000_000));
        assert!(!state.usage.context_nearly_full());

        let reported = state.reported_usage.as_ref().unwrap();
        assert_eq!(reported.estimated_cost_usd, Some(0.1295745));
        assert_eq!(reported.models.len(), 2);
        assert_eq!(reported.models[1].0, "claude-sonnet-5-5");
        assert_eq!(reported.models[1].1.cache_read, 127725);
    }

    #[test]
    fn test_turn_usage() {
        let state = {
            let mut state = SessionState::new(AgentKind::ClaudeCode);
            // Up to the end of the first turn.
            for line in SESSION.lines().take(13) {
                state.apply_all(parse_line(line));
            }
            state
        };
        assert_eq!(state.usage.turn, state.usage.session);
        assert_eq!(state.usage.turn.output, 305 + 50 + 3653);
    }

    #[test]
    fn test_sidechain_usage_counts_for_the_session_only() {
        let mut state = SessionState::new(AgentKind::ClaudeCode);
        state.apply_all(parse_line(
            r#"{"type":"assistant","message":{"model":"claude-opus-5-5","id":"m1","content":[],"usage":{"input_tokens":10,"output_tokens":5,"cache_read_input_tokens":100,"cache_creation_input_tokens":0}}}"#,
        ));
        state.apply_all(parse_line(
            r#"{"type":"assistant","isSidechain":true,"message":{"model":"claude-haiku-5-5","id":"m2","content":[{"type":"text","text":"sub"}],"usage":{"input_tokens":1000,"output_tokens":1,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}"#,
        ));
        assert_eq!(state.usage.session.input, 1010);
        assert_eq!(state.usage.turn.input, 10);
        assert_eq!(state.usage.context_tokens, Some(110));
        assert_eq!(state.usage.model.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(state.last_assistant_message(), None);
    }

    #[test]
    fn test_context_window() {
        assert_eq!(context_window("claude-opus-5-5"), Some(1_000_000));
        assert_eq!(context_window("claude-haiku-4-5-20251001"), Some(200_000));
        assert_eq!(context_window("claude-sonnet-4-5[1m]"), Some(1_000_000));
        assert_eq!(context_window("some-new-model"), None);

        let usage = UsageSummary {
            context_tokens: Some(170_000),
            context_window: Some(200_000),
            ..Default::default()
        };
        assert_eq!(usage.context_fraction(), Some(0.85));
        assert!(usage.context_nearly_full());
        let unknown = UsageSummary {
            context_tokens: Some(170_000),
            ..Default::default()
        };
        assert!(!unknown.context_nearly_full());

        // More context than the model is known to take: the window is larger.
        let mut state = SessionState::new(AgentKind::ClaudeCode);
        state.apply_all(parse_line(
            r#"{"type":"assistant","message":{"model":"claude-sonnet-4-5-20250929","id":"m","content":[],"usage":{"input_tokens":1,"output_tokens":1,"cache_read_input_tokens":300000,"cache_creation_input_tokens":0}}}"#,
        ));
        assert_eq!(state.usage.context_window, None);
        state.apply_all(parse_line(
            r#"{"type":"assistant","requestedModel":"sonnet[1m]","message":{"model":"claude-sonnet-4-5-20250929","id":"m2","content":[],"usage":{"input_tokens":1,"output_tokens":1,"cache_read_input_tokens":300000,"cache_creation_input_tokens":0}}}"#,
        ));
        assert_eq!(state.usage.context_window, Some(1_000_000));
    }

    #[test]
    fn test_compaction_resets_context() {
        let mut state = session();
        state.apply_all(parse_line(
            r#"{"type":"system","subtype":"compact_boundary","compactMetadata":{"trigger":"auto","preTokens":257985,"postTokens":8019}}"#,
        ));
        assert_eq!(state.usage.context_tokens, Some(8019));
    }

    #[test]
    fn test_bad_lines_are_skipped() {
        for line in [
            "",
            "not json",
            "{\"type\":\"assistant\"",
            "[1,2]",
            "{\"type\":\"assistant\",\"message\":\"text\"}",
            "{\"type\":\"user\",\"message\":{\"content\":42}}",
            "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"tool_use\"}]}}",
            "{\"type\":\"ai-title\"}",
            "{\"type\":\"cost-state\",\"modelUsage\":[]}",
        ] {
            let entries = parse_line(line);
            assert!(
                entries
                    .iter()
                    .all(|entry| matches!(entry, TranscriptEntry::ReportedUsage(_))),
                "{line}: {entries:?}"
            );
        }
    }

    #[test]
    fn test_project_dir_name() {
        assert_eq!(
            project_dir_name(Path::new(r"G:\GitWorkProject\Godot\godot_engine")),
            "G--GitWorkProject-Godot-godot-engine"
        );
        assert_eq!(
            project_dir_name(Path::new(r"D:\GodotEngine")),
            "D--GodotEngine"
        );
        assert_eq!(
            project_dir_name(Path::new("/home/me/my.app")),
            "-home-me-my-app"
        );
        assert_eq!(project_dir_name(Path::new(r"D:\项目")), "D----");
    }

    #[test]
    #[cfg(windows)]
    fn test_native_path() {
        assert_eq!(
            native_path(Path::new("/d/GodotEngine/src")),
            PathBuf::from(r"D:\GodotEngine\src")
        );
        assert_eq!(native_path(Path::new("/c")), PathBuf::from(r"C:\"));
        assert_eq!(
            native_path(Path::new("/usr/bin")),
            PathBuf::from("/usr/bin")
        );
        assert_eq!(native_path(Path::new(r"D:\x")), PathBuf::from(r"D:\x"));
    }

    #[test]
    fn test_find_session() {
        let config = tempfile::tempdir().unwrap();
        let working_directory = if cfg!(windows) {
            PathBuf::from(r"D:\Work\my_app")
        } else {
            PathBuf::from("/work/my_app")
        };
        let projects = config.path().join("projects");
        let dir = projects.join(project_dir_name(&working_directory));
        std::fs::create_dir_all(&dir).unwrap();
        let other_dir = projects.join("elsewhere");
        std::fs::create_dir_all(&other_dir).unwrap();
        let old = "11111111-1111-1111-1111-111111111111";
        let new = "22222222-2222-2222-2222-222222222222";
        let resumed = "33333333-3333-3333-3333-333333333333";
        std::fs::write(dir.join(format!("{old}.jsonl")), "{}\n").unwrap();
        let started_at = SystemTime::now();
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(dir.join(format!("{new}.jsonl")), "{}\n").unwrap();
        std::fs::write(other_dir.join(format!("{resumed}.jsonl")), "{}\n").unwrap();

        let probe = TerminalProbe {
            working_directory: Some(working_directory),
            started_at: Some(started_at + START_SLACK),
            ..Default::default()
        };
        let found = find_session_in(config.path(), &probe).unwrap();
        assert_eq!(found.session_id, new);
        assert_eq!(found.matched_by, MatchedBy::Recent { ambiguous: false });

        // Started earlier: both were written since.
        let probe = TerminalProbe {
            started_at: Some(started_at - Duration::from_secs(60)),
            ..probe
        };
        let found = find_session_in(config.path(), &probe).unwrap();
        assert_eq!(found.session_id, new);
        assert_eq!(found.matched_by, MatchedBy::Recent { ambiguous: true });

        // Nothing written since it started.
        let probe = TerminalProbe {
            started_at: Some(SystemTime::now() + Duration::from_secs(60)),
            ..probe
        };
        assert_eq!(find_session_in(config.path(), &probe), None);

        // Reported by the agent, even from another directory.
        let probe = TerminalProbe {
            reported_session_id: Some(resumed.to_string()),
            ..probe
        };
        let found = find_session_in(config.path(), &probe).unwrap();
        assert_eq!(found.session_id, resumed);
        assert_eq!(found.matched_by, MatchedBy::Reported);
        assert_eq!(found.path, other_dir.join(format!("{resumed}.jsonl")));

        // Ids that aren't ids aren't looked for.
        let probe = TerminalProbe {
            reported_session_id: Some("../x".to_string()),
            started_at: None,
            ..probe
        };
        let found = find_session_in(config.path(), &probe).unwrap();
        assert_eq!(found.session_id, new);

        let listed: Vec<String> = transcripts_in(&dir)
            .into_iter()
            .map(|location| location.session_id)
            .collect();
        assert_eq!(listed, [new, old]);
    }

    #[test]
    fn test_describe_transcript() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        std::fs::write(
            &path,
            "{\"type\":\"last-prompt\",\"lastPrompt\":\"first\"}\n{\"type\":\"last-prompt\",\"lastPrompt\":\"second\"}\n",
        )
        .unwrap();
        assert_eq!(describe_transcript(&path).as_deref(), Some("second"));
        std::fs::write(&path, format!("{SESSION}\n")).unwrap();
        assert_eq!(
            describe_transcript(&path).as_deref(),
            Some("Build explained")
        );
    }

    /// Reads a real transcript: `SOLO_TRANSCRIPT=<path> cargo test -p
    /// agent_sessions real_transcript -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn test_real_transcript() {
        let Some(path) = std::env::var_os("SOLO_TRANSCRIPT") else {
            return;
        };
        let location = SessionLocation {
            agent: AgentKind::ClaudeCode,
            session_id: String::new(),
            path: PathBuf::from(path),
            modified: None,
            matched_by: MatchedBy::Chosen,
        };
        let state = SessionState::load(&location).unwrap();
        let mut tools = collections::HashMap::<&str, usize>::default();
        for call in &state.tool_calls {
            *tools.entry(call.name.as_str()).or_default() += 1;
        }
        println!("title: {:?}", state.title);
        println!("messages: {}", state.messages.len());
        println!("tool calls: {tools:?}");
        println!("file changes: {}", state.file_changes.len());
        println!("usage: {:?}", state.usage);
        println!("reported: {:?}", state.reported_usage);
        let last = state
            .last_assistant_message()
            .map(|message| message.text.as_str());
        println!("last answer: {} chars", last.map_or(0, str::len));
    }
}
