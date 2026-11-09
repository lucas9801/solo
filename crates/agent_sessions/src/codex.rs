//! SOLO: Codex's sessions: where they are and how to read them. Everything
//! about its file formats is here.
//!
//! Each session is `~/.codex/sessions/<year>/<month>/<day>/rollout-<time>-<session id>.jsonl`
//! (`CODEX_HOME` replaces `~/.codex`), one JSON record per line, only ever
//! appended to. Seen, not documented; the records read here, by `type` (and
//! `payload.type`):
//! - `session_meta`: the first record, with `payload.id` (the session id the
//!   hooks report) and `payload.cwd`, where it started.
//! - `turn_context`: each turn's `payload.model` and `payload.cwd`.
//! - `event_msg` / `user_message`: a prompt the user sent, `payload.message`.
//!   (`response_item` user messages also carry context Codex adds, so they're
//!   skipped.)
//! - `response_item` / `message` with `role` `assistant`: the agent's text, in
//!   `output_text` content.
//! - `response_item` / `function_call` (`name`, `arguments`, a JSON string),
//!   `custom_tool_call` (`name`, `input`) and `local_shell_call`, with their
//!   `*_output`s, by `call_id`.
//! - `event_msg` / `patch_apply_end`: a patch applied, `payload.changes` by
//!   path.
//! - `event_msg` / `token_count`: `payload.info.last_token_usage` (the last
//!   request: `input_tokens`, cached ones included, `cached_input_tokens`,
//!   `cache_write_input_tokens`, `output_tokens`),
//!   `payload.info.total_token_usage` and `payload.info.model_context_window`.
//! - `compacted`, `event_msg` / `context_compacted`: the conversation was
//!   compacted.
//!
//! Others are skipped, as are fields not named here.

use std::{
    io::{BufRead as _, BufReader, Read as _, Seek as _, SeekFrom},
    path::{Path, PathBuf},
    time::Duration,
};

use serde_json::Value;

use crate::{
    AgentAdapter, AgentKind, MatchedBy, Message, Role, SessionLocation, TerminalProbe,
    TokenUsage, ToolCall, ToolKind, ToolOutcome, TranscriptEntry,
};

/// A session written to this long before the agent started (file times and
/// process start times differ a little) still counts.
const START_SLACK: Duration = Duration::from_secs(5);
/// How much of the end of a session is read to describe it.
const DESCRIBE_TAIL_BYTES: u64 = 256 * 1024;
/// How many of the newest sessions are looked through for a directory's.
const MAX_SESSIONS_LOOKED_AT: usize = 300;
/// Strings in tool inputs longer than this are cut short.
const MAX_INPUT_STRING_CHARS: usize = 1000;

pub struct Codex;

impl AgentAdapter for Codex {
    fn kind(&self) -> AgentKind {
        AgentKind::Codex
    }

    fn runs_program(&self, program: &str) -> bool {
        program == "codex"
    }

    fn find_session(&self, probe: &TerminalProbe) -> Option<SessionLocation> {
        find_session_in(&sessions_dir()?, probe)
    }

    fn sessions_in(&self, working_directory: &Path) -> Vec<SessionLocation> {
        let Some(dir) = sessions_dir() else {
            return Vec::new();
        };
        sessions_in(&dir, working_directory)
    }

    fn describe_session(&self, location: &SessionLocation) -> Option<String> {
        describe_session(&location.path)
    }

    fn parse_line(&self, line: &str) -> Vec<TranscriptEntry> {
        parse_line(line)
    }

    fn context_window(&self, _model: &str) -> Option<u64> {
        // Codex records it with the usage (`model_context_window`).
        None
    }
}

fn sessions_dir() -> Option<PathBuf> {
    let home = match std::env::var_os("CODEX_HOME").filter(|dir| !dir.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => util::paths::home_dir().join(".codex"),
    };
    let dir = home.join("sessions");
    dir.is_dir().then_some(dir)
}

/// The session files, newest first by their path (which starts with the
/// date and time).
fn session_files(sessions: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut dirs = vec![sessions.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                dirs.push(path);
            } else if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("rollout-") && name.ends_with(".jsonl"))
            {
                files.push(path);
            }
        }
    }
    files.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
    files
}

/// The session id at the end of a session file's name:
/// `rollout-2026-10-28T22-42-11-<uuid>.jsonl`.
fn session_id_of(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    let id = stem.get(stem.len().checked_sub(36)?..)?;
    is_session_id(id).then(|| id.to_string())
}

fn is_session_id(id: &str) -> bool {
    id.len() == 36
        && id
            .chars()
            .all(|character| character.is_ascii_hexdigit() || character == '-')
}

fn location(path: PathBuf, matched_by: MatchedBy) -> Option<SessionLocation> {
    Some(SessionLocation {
        agent: AgentKind::Codex,
        session_id: session_id_of(&path)?,
        modified: std::fs::metadata(&path)
            .and_then(|metadata| metadata.modified())
            .ok(),
        path,
        matched_by,
    })
}

/// Where a session started, from its first record.
fn session_cwd(path: &Path) -> Option<PathBuf> {
    let file = std::fs::File::open(path).ok()?;
    let mut first = String::new();
    BufReader::new(file).read_line(&mut first).ok()?;
    let record: Value = serde_json::from_str(&first).ok()?;
    (record.get("type")?.as_str()? == "session_meta")
        .then(|| record.get("payload")?.get("cwd")?.as_str().map(PathBuf::from))
        .flatten()
}

/// Whether two directories are the same, ignoring a trailing separator, and
/// case on Windows.
fn same_directory(a: &Path, b: &Path) -> bool {
    let normalize = |path: &Path| {
        let text = path.to_string_lossy();
        let text = text.trim_end_matches(['/', '\\']);
        if cfg!(windows) {
            text.to_lowercase()
        } else {
            text.to_string()
        }
    };
    normalize(a) == normalize(b)
}

fn find_session_in(sessions: &Path, probe: &TerminalProbe) -> Option<SessionLocation> {
    let files = session_files(sessions);
    if let Some(session_id) = probe
        .reported_session_id
        .as_deref()
        .filter(|id| is_session_id(id))
        && let Some(path) = files
            .iter()
            .find(|path| session_id_of(path).as_deref() == Some(session_id))
    {
        return location(path.clone(), MatchedBy::Reported);
    }
    let working_directory = probe.working_directory.as_deref()?;
    let since = probe
        .started_at
        .map(|started_at| started_at.checked_sub(START_SLACK).unwrap_or(started_at));
    let mut candidates = files
        .into_iter()
        .take(MAX_SESSIONS_LOOKED_AT)
        .filter_map(|path| location(path, MatchedBy::Recent { ambiguous: false }))
        .filter(|location| {
            since.is_none_or(|since| location.modified.is_some_and(|modified| modified >= since))
        })
        .filter(|location| {
            session_cwd(&location.path)
                .is_some_and(|cwd| same_directory(&cwd, working_directory))
        });
    let mut newest = candidates.next()?;
    newest.matched_by = MatchedBy::Recent {
        ambiguous: since.is_none() || candidates.next().is_some(),
    };
    Some(newest)
}

fn sessions_in(sessions: &Path, working_directory: &Path) -> Vec<SessionLocation> {
    let mut locations: Vec<SessionLocation> = session_files(sessions)
        .into_iter()
        .take(MAX_SESSIONS_LOOKED_AT)
        .filter(|path| {
            session_cwd(path).is_some_and(|cwd| same_directory(&cwd, working_directory))
        })
        .filter_map(|path| location(path, MatchedBy::Chosen))
        .collect();
    locations.sort_by_key(|location| std::cmp::Reverse(location.modified));
    locations
}

/// The last prompt of a session, from the end of its file.
fn describe_session(path: &Path) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let length = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(length.saturating_sub(DESCRIBE_TAIL_BYTES)))
        .ok()?;
    let mut tail = Vec::new();
    file.read_to_end(&mut tail).ok()?;
    let tail = String::from_utf8_lossy(&tail);
    tail.lines().rev().find_map(|line| {
        parse_line(line).into_iter().find_map(|entry| match entry {
            TranscriptEntry::Message(message) if message.role == Role::User => {
                Some(first_line(&message.text))
            }
            _ => None,
        })
    })
}

/// The first line of a prompt, shortened, as a session's title.
pub(crate) fn first_line(text: &str) -> String {
    let line = text.trim().lines().next().unwrap_or_default().trim();
    let mut short: String = line.chars().take(80).collect();
    if line.chars().count() > 80 {
        short.push('…');
    }
    short
}

fn parse_line(line: &str) -> Vec<TranscriptEntry> {
    let Ok(record) = serde_json::from_str::<Value>(line) else {
        // A line still being written, or not JSON: skipped.
        return Vec::new();
    };
    let timestamp = record
        .get("timestamp")
        .and_then(Value::as_str)
        .map(str::to_string);
    let Some(payload) = record.get("payload") else {
        return Vec::new();
    };
    let str_field = |name: &str| payload.get(name).and_then(Value::as_str);
    match (
        record.get("type").and_then(Value::as_str),
        str_field("type"),
    ) {
        (Some("turn_context"), _) => match str_field("model") {
            Some(model) => vec![TranscriptEntry::ModelInfo {
                model: Some(model.to_string()),
                context_window: None,
            }],
            None => Vec::new(),
        },
        (Some("event_msg"), Some("user_message")) => match str_field("message") {
            Some(text) if !text.trim().is_empty() => {
                vec![TranscriptEntry::Message(Message {
                    role: Role::User,
                    text: text.to_string(),
                    id: None,
                    api_message_id: None,
                    timestamp,
                    sidechain: false,
                })]
            }
            _ => Vec::new(),
        },
        (Some("response_item"), Some("message")) if str_field("role") == Some("assistant") => {
            let text = payload
                .get("content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|content| content.get("type").and_then(Value::as_str) == Some("output_text"))
                .filter_map(|content| content.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n\n");
            if text.trim().is_empty() {
                return Vec::new();
            }
            vec![TranscriptEntry::Message(Message {
                role: Role::Assistant,
                text,
                id: str_field("id").map(str::to_string),
                api_message_id: str_field("id").map(str::to_string),
                timestamp,
                sidechain: false,
            })]
        }
        (Some("response_item"), Some(kind @ ("function_call" | "custom_tool_call" | "local_shell_call"))) => {
            let Some(id) = str_field("call_id").or_else(|| str_field("id")) else {
                return Vec::new();
            };
            let name = str_field("name").unwrap_or(match kind {
                "local_shell_call" => "shell",
                _ => "tool",
            });
            let input = match kind {
                "function_call" => str_field("arguments")
                    .and_then(|arguments| serde_json::from_str::<Value>(arguments).ok())
                    .unwrap_or(Value::Null),
                "custom_tool_call" => Value::String(str_field("input").unwrap_or_default().to_string()),
                _ => payload.get("action").cloned().unwrap_or(Value::Null),
            };
            let command = command_of(&input);
            vec![TranscriptEntry::ToolCall(ToolCall {
                id: id.to_string(),
                name: name.to_string(),
                kind: tool_kind(name, command.is_some()),
                file_path: None,
                command,
                description: None,
                input: shortened(input),
                timestamp,
                sidechain: false,
                outcome: None,
            })]
        }
        (
            Some("response_item"),
            Some("function_call_output" | "custom_tool_call_output" | "local_shell_call_output"),
        ) => match str_field("call_id") {
            Some(call_id) => vec![TranscriptEntry::ToolResult {
                tool_call_id: call_id.to_string(),
                outcome: ToolOutcome::Succeeded,
            }],
            None => Vec::new(),
        },
        (Some("event_msg"), Some("patch_apply_end")) => {
            let call_id = str_field("call_id").unwrap_or_default();
            let success = payload.get("success").and_then(Value::as_bool);
            payload
                .get("changes")
                .and_then(Value::as_object)
                .into_iter()
                .flatten()
                .map(|(path, change)| {
                    let added = change.get("type").and_then(Value::as_str) == Some("add");
                    TranscriptEntry::ToolCall(ToolCall {
                        id: format!("{call_id}:{path}"),
                        name: "apply_patch".to_string(),
                        kind: if added { ToolKind::Write } else { ToolKind::Edit },
                        file_path: Some(PathBuf::from(path)),
                        command: None,
                        description: None,
                        input: Value::Null,
                        timestamp: timestamp.clone(),
                        sidechain: false,
                        outcome: success.map(|success| {
                            if success {
                                ToolOutcome::Succeeded
                            } else {
                                ToolOutcome::Failed
                            }
                        }),
                    })
                })
                .collect()
        }
        (Some("event_msg"), Some("token_count")) => {
            let Some(info) = payload.get("info").filter(|info| !info.is_null()) else {
                return Vec::new();
            };
            let number = |usage: &Value, name: &str| usage.get(name).and_then(Value::as_u64).unwrap_or(0);
            let mut entries = Vec::new();
            if let Some(window) = info.get("model_context_window").and_then(Value::as_u64) {
                entries.push(TranscriptEntry::ModelInfo {
                    model: None,
                    context_window: Some(window),
                });
            }
            // Codex repeats a count now and then; the session's running
            // total tells one request from another.
            if let (Some(last), Some(total)) =
                (info.get("last_token_usage"), info.get("total_token_usage"))
            {
                let input = number(last, "input_tokens");
                let cache_read = number(last, "cached_input_tokens");
                let cache_creation = number(last, "cache_write_input_tokens");
                entries.push(TranscriptEntry::Usage {
                    api_message_id: format!("total-{}", number(total, "total_tokens")),
                    model: None,
                    usage: TokenUsage {
                        input: input.saturating_sub(cache_read + cache_creation),
                        output: number(last, "output_tokens"),
                        cache_read,
                        cache_creation,
                    },
                    sidechain: false,
                });
            }
            entries
        }
        (Some("compacted"), _) | (Some("event_msg"), Some("context_compacted")) => {
            vec![TranscriptEntry::Compacted { tokens_after: None }]
        }
        _ => Vec::new(),
    }
}

/// The command a tool call runs, for the shell tools of Codex's versions:
/// `shell` (`command`, a list), `shell_command` (`command`), `exec_command`
/// (`cmd`), `local_shell_call` (`command`, a list).
fn command_of(input: &Value) -> Option<String> {
    let field = input.get("command").or_else(|| input.get("cmd"))?;
    let command = match field {
        Value::String(command) => command.clone(),
        Value::Array(parts) => {
            let parts: Vec<&str> = parts.iter().filter_map(Value::as_str).collect();
            // `["powershell.exe", "-Command", "ls"]`: what it runs.
            match parts.as_slice() {
                [_, flag, command]
                    if matches!(*flag, "-c" | "-lc" | "-Command" | "/c" | "/C") =>
                {
                    command.to_string()
                }
                _ => parts.join(" "),
            }
        }
        _ => return None,
    };
    (!command.trim().is_empty()).then_some(command)
}

fn tool_kind(name: &str, has_command: bool) -> ToolKind {
    match name {
        "spawn_agent" | "followup_task" => ToolKind::Delegate,
        "web_search" | "web_fetch" => ToolKind::Fetch,
        "exec" | "shell" | "shell_command" | "exec_command" | "local_shell" => ToolKind::Execute,
        _ if has_command => ToolKind::Execute,
        _ => ToolKind::Other,
    }
}

/// `value` with long strings cut short.
fn shortened(value: Value) -> Value {
    match value {
        Value::String(text) if text.chars().count() > MAX_INPUT_STRING_CHARS => {
            let mut short: String = text.chars().take(MAX_INPUT_STRING_CHARS).collect();
            short.push('…');
            Value::String(short)
        }
        Value::Array(items) => Value::Array(items.into_iter().map(shortened).collect()),
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, value)| (key, shortened(value)))
                .collect(),
        ),
        value => value,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SessionState;

    /// A made-up session in the shape of a real Codex one.
    const SESSION: &str = r#"{"timestamp":"2026-11-09T10:00:00.000Z","type":"session_meta","payload":{"id":"01a182bf-336d-7c13-9879-86020812dc44","cwd":"G:\\Work\\game","cli_version":"0.130.0"}}
{"timestamp":"2026-11-09T10:00:01.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>cwd</environment_context>"}]}}
{"timestamp":"2026-11-09T10:00:01.000Z","type":"turn_context","payload":{"turn_id":"t1","cwd":"G:\\Work\\game","model":"gpt-5.5-codex"}}
{"timestamp":"2026-11-09T10:00:01.100Z","type":"event_msg","payload":{"type":"user_message","message":"Fix the save bug\nIt crashes on load.","images":[]}}
{"timestamp":"2026-11-09T10:00:02.000Z","type":"event_msg","payload":{"type":"token_count","info":null}}
{"timestamp":"2026-11-09T10:00:03.000Z","type":"response_item","payload":{"type":"function_call","name":"shell_command","arguments":"{\"command\":\"git status\",\"workdir\":\"G:\\\\Work\\\\game\"}","call_id":"call_1"}}
{"timestamp":"2026-11-09T10:00:04.000Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_1","output":"clean"}}
{"timestamp":"2026-11-09T10:00:05.000Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":12000,"cached_input_tokens":8000,"cache_write_input_tokens":0,"output_tokens":300,"total_tokens":12300},"last_token_usage":{"input_tokens":12000,"cached_input_tokens":8000,"cache_write_input_tokens":0,"output_tokens":300,"total_tokens":12300},"model_context_window":258400}}}
{"timestamp":"2026-11-09T10:00:05.000Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":12000,"cached_input_tokens":8000,"cache_write_input_tokens":0,"output_tokens":300,"total_tokens":12300},"last_token_usage":{"input_tokens":12000,"cached_input_tokens":8000,"cache_write_input_tokens":0,"output_tokens":300,"total_tokens":12300},"model_context_window":258400}}}
{"timestamp":"2026-11-09T10:00:06.000Z","type":"event_msg","payload":{"type":"patch_apply_end","call_id":"call_2","success":true,"changes":{"G:\\Work\\game\\save.rs":{"type":"update","unified_diff":"@@"},"G:\\Work\\game\\load.rs":{"type":"add","unified_diff":"@@"}}}}
{"timestamp":"2026-11-09T10:00:07.000Z","type":"response_item","payload":{"type":"message","id":"msg_1","role":"assistant","content":[{"type":"output_text","text":"Fixed it."}]}}
{"timestamp":"2026-11-09T10:00:08.000Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":25000,"cached_input_tokens":20000,"cache_write_input_tokens":0,"output_tokens":500,"total_tokens":25500},"last_token_usage":{"input_tokens":13000,"cached_input_tokens":12000,"cache_write_input_tokens":0,"output_tokens":200,"total_tokens":13200},"model_context_window":258400}}}
{"timestamp":"2026-11-09T10:00:09.000Z","type":"future_record","payload":{"type":"whatever"}}
{"timestamp":"2026-11-09T10:00:10.000Z","type":"event_msg","payload":{"type":"user_message","message":"Thanks"#;

    fn state() -> SessionState {
        let mut state = SessionState::new(AgentKind::Codex);
        for line in SESSION.lines() {
            state.apply_all(parse_line(line));
        }
        state
    }

    #[test]
    fn test_parse_session() {
        let state = state();
        // The prompt the user typed, not the context Codex added; the last
        // line is still being written.
        let prompts: Vec<&str> = state
            .messages
            .iter()
            .filter(|message| message.role == Role::User)
            .map(|message| message.text.as_str())
            .collect();
        assert_eq!(prompts, ["Fix the save bug\nIt crashes on load."]);
        assert_eq!(state.title.as_deref(), Some("Fix the save bug"));
        assert_eq!(
            state.last_assistant_message().map(|message| message.text.as_str()),
            Some("Fixed it.")
        );
        // The command, and the files the patch changed.
        assert_eq!(state.tool_calls[0].command.as_deref(), Some("git status"));
        assert_eq!(state.tool_calls[0].kind, ToolKind::Execute);
        assert_eq!(state.tool_calls[0].outcome, Some(ToolOutcome::Succeeded));
        let changed: Vec<(PathBuf, ToolKind)> = state
            .file_changes
            .iter()
            .map(|change| (change.path.clone(), change.kind))
            .collect();
        assert!(changed.contains(&(PathBuf::from("G:\\Work\\game\\save.rs"), ToolKind::Edit)));
        assert!(changed.contains(&(PathBuf::from("G:\\Work\\game\\load.rs"), ToolKind::Write)));
    }

    #[test]
    fn test_usage() {
        let usage = state().usage;
        // Two requests; the repeated count counts once.
        assert_eq!(usage.session.total_input(), 25_000);
        assert_eq!(usage.session.output, 500);
        assert_eq!(usage.session.cache_read, 20_000);
        assert_eq!(usage.turn.total(), 25_500);
        // The context is the last request's; the window as Codex records it.
        assert_eq!(usage.context_tokens, Some(13_000));
        assert_eq!(usage.context_window, Some(258_400));
        assert_eq!(usage.model.as_deref(), Some("gpt-5.5-codex"));
    }

    #[test]
    fn test_session_files() {
        let dir = tempfile::tempdir().unwrap();
        let day = dir.path().join("2026").join("11").join("09");
        std::fs::create_dir_all(&day).unwrap();
        let older = day.join("rollout-2026-11-09T09-00-00-01a182bf-336d-7c13-9879-86020812dc44.jsonl");
        let newer = day.join("rollout-2026-11-09T10-00-00-0f6ed4b2-755b-4e18-8bd1-f60252548036.jsonl");
        std::fs::write(&older, format!("{}\n", SESSION.lines().next().unwrap())).unwrap();
        std::fs::write(
            &newer,
            r#"{"type":"session_meta","payload":{"id":"x","cwd":"G:\\Other"}}
"#,
        )
        .unwrap();
        std::fs::write(day.join("notes.txt"), "").unwrap();
        assert_eq!(session_files(dir.path()), [newer, older.clone()]);
        assert_eq!(
            session_id_of(&older).as_deref(),
            Some("01a182bf-336d-7c13-9879-86020812dc44")
        );

        // By the reported id.
        let probe = |reported: Option<&str>, directory: &str| TerminalProbe {
            working_directory: Some(PathBuf::from(directory)),
            command: Some("codex".into()),
            started_at: None,
            shell_pid: None,
            reported_session_id: reported.map(str::to_string),
        };
        let found = find_session_in(
            dir.path(),
            &probe(Some("01a182bf-336d-7c13-9879-86020812dc44"), "G:\\Other"),
        )
        .unwrap();
        assert_eq!(found.path, older);
        assert_eq!(found.matched_by, MatchedBy::Reported);
        // By where it started, ignoring case on Windows.
        let found = find_session_in(dir.path(), &probe(None, "G:\\Work\\game\\")).unwrap();
        assert_eq!(found.path, older);
        assert_eq!(sessions_in(dir.path(), Path::new("G:\\Other")).len(), 1);
        assert!(find_session_in(dir.path(), &probe(None, "G:\\Nowhere")).is_none());
    }
}
