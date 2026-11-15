//! SOLO: Codex's model requests, from the OpenTelemetry log events Codex
//! sends when its config has an `[otel]` exporter. Solo gives the sessions
//! it starts one for that session only (`-c otel.exporter=...`, see
//! [`Receiver::codex_config_overrides`]) that sends to [`Receiver`], an OTLP
//! endpoint on this machine, unless the user set up telemetry of their own.
//!
//! Codex times each request itself: its `codex.sse_event` event with
//! `event.kind` `response.completed` has `ttft_ms` (from the request to the
//! first output item) and `output_token_count`, and comes after the event
//! for sending the request, `codex.websocket_request` (over a WebSocket,
//! Codex's default) or `codex.api_request` (over HTTP, where the time to
//! first token starts once the response's headers came, after its
//! `duration_ms`). Every event has the session's `conversation.id`. Read
//! from Codex's source (`codex-rs/otel/src/events/session_telemetry.rs`,
//! `codex-rs/core/src/client.rs`), not from Codex running.
//!
//! Only these numbers are kept, in memory; everything else an event has
//! (the account, the prompt's length, tool output) is dropped as it comes.

use std::{
    io::{BufRead as _, BufReader, Read as _, Write as _},
    net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use collections::HashMap;
use serde_json::Value;

use crate::{
    ResponseSummary, ResponseTiming, ResponseTimings,
    response_timing::{effort_level, millis_between},
};

/// The header that carries the receiver's token.
pub const TOKEN_HEADER: &str = "x-solo-token";
/// The path Codex sends log events to.
const LOGS_PATH: &str = "/v1/logs";
/// Longest request head and body taken.
const MAX_HEAD_BYTES: usize = 64 * 1024;
const MAX_BODY_BYTES: usize = 32 * 1024 * 1024;
/// How many connections are served at once.
const MAX_CONNECTIONS: usize = 16;
/// How long an idle connection stays open.
const IDLE_TIMEOUT: Duration = Duration::from_secs(120);
/// How many sessions are kept; the least recently heard from go first.
const MAX_CONVERSATIONS: usize = 64;

/// What Codex's events said about each session.
#[derive(Debug, Default)]
pub struct CodexTelemetry {
    conversations: HashMap<String, Conversation>,
    /// Counts events, to tell which session was heard from last.
    clock: u64,
}

#[derive(Debug, Default)]
struct Conversation {
    /// When the request being answered was sent (ms since the epoch), and
    /// how long its response's headers took when that's not in `ttft_ms`.
    sent: Option<(i64, u64)>,
    timings: ResponseTimings,
    requests: u64,
    effort: Option<String>,
    heard_at: u64,
}

/// One log event, as far as it's read.
#[derive(Clone, Debug, Default, PartialEq)]
struct Event {
    name: String,
    kind: Option<String>,
    conversation: String,
    at_ms: Option<i64>,
    ttft_ms: Option<u64>,
    output_tokens: Option<u64>,
    duration_ms: Option<u64>,
    effort: Option<String>,
}

impl CodexTelemetry {
    /// Takes an OTLP/JSON logs export (`ExportLogsServiceRequest`).
    pub fn ingest(&mut self, export: &Value) {
        let mut events = events_of(export);
        events.sort_by_key(|event| event.at_ms);
        for event in events {
            self.apply(event);
        }
    }

    fn apply(&mut self, event: Event) {
        self.clock += 1;
        if !self.conversations.contains_key(&event.conversation)
            && self.conversations.len() >= MAX_CONVERSATIONS
            && let Some(oldest) = self
                .conversations
                .iter()
                .min_by_key(|(_, conversation)| conversation.heard_at)
                .map(|(id, _)| id.clone())
        {
            self.conversations.remove(&oldest);
        }
        let conversation = self
            .conversations
            .entry(event.conversation.clone())
            .or_default();
        conversation.heard_at = self.clock;
        match event.name.as_str() {
            "codex.websocket_request" => {
                conversation.sent = event.at_ms.map(|at_ms| (at_ms, 0));
            }
            "codex.api_request" => {
                conversation.sent = event.at_ms.zip(event.duration_ms);
            }
            "codex.sse_event" if event.kind.as_deref() == Some("response.completed") => {
                if let Some(effort) = event.effort {
                    conversation.effort = Some(effort);
                }
                // Only a response to a request seen being sent: otherwise
                // it's unknown whether its first token waited for headers.
                let Some((sent_at_ms, headers_ms)) = conversation.sent.take() else {
                    return;
                };
                let Some(ttft_ms) = event.ttft_ms else {
                    return;
                };
                let generation = event.at_ms.and_then(|at_ms| {
                    let first_at = sent_at_ms.checked_add(i64::try_from(ttft_ms).ok()?)?;
                    millis_between(first_at, at_ms)
                });
                conversation.requests += 1;
                let key = conversation.requests.to_string();
                conversation.timings.record(
                    &key,
                    ResponseTiming {
                        first_token: Some(Duration::from_millis(ttft_ms + headers_ms)),
                        output_tokens: event.output_tokens,
                        generation,
                    },
                    false,
                );
            }
            _ => {}
        }
    }

    /// How fast the model answered session `conversation_id`'s requests.
    pub fn summary(&self, conversation_id: &str) -> Option<ResponseSummary> {
        let conversation = self.conversations.get(conversation_id)?;
        (!conversation.timings.is_empty()).then(|| conversation.timings.summary())
    }

    /// The reasoning effort of session `conversation_id`'s latest request.
    pub fn effort(&self, conversation_id: &str) -> Option<String> {
        self.conversations.get(conversation_id)?.effort.clone()
    }
}

fn events_of(export: &Value) -> Vec<Event> {
    let list = |value: &Value, name: &str| -> Vec<Value> {
        value
            .get(name)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    let mut events = Vec::new();
    for resource in list(export, "resourceLogs") {
        for scope in list(&resource, "scopeLogs") {
            for record in list(&scope, "logRecords") {
                if let Some(event) = event_of(&record) {
                    events.push(event);
                }
            }
        }
    }
    events
}

fn event_of(record: &Value) -> Option<Event> {
    let attributes = record.get("attributes")?.as_array()?;
    let attribute = |key: &str| -> Option<&Value> {
        let value = attributes
            .iter()
            .find(|attribute| attribute.get("key").and_then(Value::as_str) == Some(key))?
            .get("value")?;
        ["stringValue", "intValue", "doubleValue", "boolValue"]
            .iter()
            .find_map(|kind| value.get(*kind))
    };
    let text = |key: &str| attribute(key).and_then(Value::as_str).map(str::to_string);
    let count = |key: &str| -> Option<u64> {
        match attribute(key)? {
            Value::Number(number) => number.as_u64(),
            Value::String(text) => text.trim().parse().ok(),
            _ => None,
        }
    };
    let name = text("event.name")?;
    if !name.starts_with("codex.") {
        return None;
    }
    let conversation = text("conversation.id").filter(|id| {
        (1..=128).contains(&id.len())
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    })?;
    let nanos = |name: &str| -> Option<i64> {
        let nanos: i64 = match record.get(name)? {
            Value::String(text) => text.parse().ok()?,
            Value::Number(number) => number.as_i64()?,
            _ => return None,
        };
        (nanos > 0).then_some(nanos / 1_000_000)
    };
    Some(Event {
        name,
        kind: text("event.kind"),
        conversation,
        at_ms: nanos("timeUnixNano").or_else(|| nanos("observedTimeUnixNano")),
        ttft_ms: count("ttft_ms"),
        output_tokens: count("output_token_count"),
        duration_ms: count("duration_ms"),
        effort: text("model_reasoning_effort")
            .as_deref()
            .and_then(effort_level),
    })
}

/// SOLO: the OTLP endpoint on this machine that the Codex sessions Solo
/// starts send their log events to. Only requests with its token are taken.
pub struct Receiver {
    port: u16,
    token: String,
    telemetry: Arc<Mutex<CodexTelemetry>>,
}

impl Receiver {
    /// Listens on a free port of the loopback interface, in the background.
    pub fn start() -> std::io::Result<Self> {
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))?;
        let port = listener.local_addr()?.port();
        let token = random_token();
        let telemetry = Arc::new(Mutex::new(CodexTelemetry::default()));
        let connections = Arc::new(AtomicUsize::new(0));
        std::thread::Builder::new()
            .name("codex-otel-receiver".to_string())
            .spawn({
                let token = token.clone();
                let telemetry = telemetry.clone();
                move || {
                    for stream in listener.incoming() {
                        let Ok(stream) = stream else {
                            continue;
                        };
                        if connections.load(Ordering::SeqCst) >= MAX_CONNECTIONS {
                            continue;
                        }
                        connections.fetch_add(1, Ordering::SeqCst);
                        let (token, telemetry, open) =
                            (token.clone(), telemetry.clone(), connections.clone());
                        let spawned = std::thread::Builder::new()
                            .name("codex-otel-connection".to_string())
                            .spawn(move || {
                                if let Err(error) = serve(stream, &token, &telemetry) {
                                    log::debug!("Codex telemetry connection: {error}");
                                }
                                open.fetch_sub(1, Ordering::SeqCst);
                            });
                        if spawned.is_err() {
                            connections.fetch_sub(1, Ordering::SeqCst);
                        }
                    }
                }
            })?;
        Ok(Self {
            port,
            token,
            telemetry,
        })
    }

    pub fn endpoint(&self) -> String {
        format!("http://127.0.0.1:{}{LOGS_PATH}", self.port)
    }

    /// The `-c` arguments that make a Codex session send its log events
    /// here: as TOML with literal (single-quoted) strings, which Windows
    /// PowerShell passes on as they are. Only the log exporter is set
    /// (`trace_exporter` and `metrics_exporter` stay the user's), and not
    /// every streamed delta is sent.
    pub fn codex_config_overrides(&self) -> Vec<String> {
        vec![
            "-c".to_string(),
            format!(
                "otel.exporter={{otlp-http={{endpoint='{}',protocol='json',headers={{{TOKEN_HEADER}='{}'}}}}}}",
                self.endpoint(),
                self.token
            ),
            "-c".to_string(),
            "otel.log_sse_deltas=false".to_string(),
        ]
    }

    pub fn summary(&self, conversation_id: &str) -> Option<ResponseSummary> {
        self.telemetry.lock().ok()?.summary(conversation_id)
    }

    pub fn effort(&self, conversation_id: &str) -> Option<String> {
        self.telemetry.lock().ok()?.effort(conversation_id)
    }
}

static RECEIVER: OnceLock<Option<Receiver>> = OnceLock::new();

/// Solo's receiver, started the first time it's asked for; `None` if it
/// couldn't be.
pub fn receiver() -> Option<&'static Receiver> {
    RECEIVER
        .get_or_init(|| {
            Receiver::start()
                .map_err(|error| {
                    log::warn!("failed to start the Codex telemetry receiver: {error}")
                })
                .ok()
        })
        .as_ref()
}

/// Solo's receiver if a Codex session was set up to send to it, without
/// starting it.
pub fn running_receiver() -> Option<&'static Receiver> {
    RECEIVER.get()?.as_ref()
}

/// Whether the user's own Codex config sets up telemetry (or an
/// administrator's requires some): then Solo's would replace it, so it's left
/// out. Any mention of `otel` counts.
pub fn user_configures_telemetry(config_files: &[std::path::PathBuf]) -> bool {
    config_files.iter().any(|path| {
        std::fs::read_to_string(path)
            .is_ok_and(|contents| contents.to_ascii_lowercase().contains("otel"))
    })
}

/// 128 unpredictable bits, in hex.
fn random_token() -> String {
    use std::hash::{BuildHasher as _, Hasher as _};
    let half = |salt: u64| {
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u64(salt);
        hasher.write_u32(std::process::id());
        if let Ok(elapsed) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
            hasher.write_u128(elapsed.as_nanos());
        }
        hasher.finish()
    };
    format!("{:016x}{:016x}", half(1), half(2))
}

/// Serves one connection's requests until it closes.
fn serve(stream: TcpStream, token: &str, telemetry: &Mutex<CodexTelemetry>) -> std::io::Result<()> {
    stream.set_read_timeout(Some(IDLE_TIMEOUT))?;
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    loop {
        let Some(request) = read_request(&mut reader)? else {
            return Ok(());
        };
        let status = if request.method != "POST" {
            "405 Method Not Allowed"
        } else if request.header(TOKEN_HEADER) != Some(token) {
            "401 Unauthorized"
        } else if request.path.split('?').next() != Some(LOGS_PATH) {
            "404 Not Found"
        } else if request
            .header("content-encoding")
            .is_some_and(|encoding| !encoding.eq_ignore_ascii_case("identity"))
        {
            "415 Unsupported Media Type"
        } else {
            match serde_json::from_slice::<Value>(&request.body) {
                Ok(export) => {
                    if let Ok(mut telemetry) = telemetry.lock() {
                        telemetry.ingest(&export);
                    }
                    "200 OK"
                }
                Err(_) => "400 Bad Request",
            }
        };
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{{}}"
        );
        writer.write_all(response.as_bytes())?;
        writer.flush()?;
        if request
            .header("connection")
            .is_some_and(|connection| connection.eq_ignore_ascii_case("close"))
        {
            return Ok(());
        }
    }
}

struct Request {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header, _)| header.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

fn invalid(message: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message.to_string())
}

/// The next request on a connection; `None` once it's closed.
fn read_request(reader: &mut impl std::io::BufRead) -> std::io::Result<Option<Request>> {
    let mut head_bytes = 0;
    let mut read_line = |reader: &mut dyn std::io::BufRead| -> std::io::Result<Option<String>> {
        let mut line = Vec::new();
        let read = reader
            .take((MAX_HEAD_BYTES - head_bytes) as u64 + 1)
            .read_until(b'\n', &mut line)?;
        if read == 0 {
            return Ok(None);
        }
        head_bytes += read;
        if head_bytes > MAX_HEAD_BYTES {
            return Err(invalid("request head too long"));
        }
        let line = String::from_utf8(line).map_err(|_| invalid("request head isn't UTF-8"))?;
        Ok(Some(line.trim_end_matches(['\r', '\n']).to_string()))
    };
    let request_line = loop {
        match read_line(reader)? {
            None => return Ok(None),
            // Blank lines before a request are allowed.
            Some(line) if line.is_empty() => continue,
            Some(line) => break line,
        }
    };
    let mut parts = request_line.split_whitespace();
    let (Some(method), Some(path)) = (parts.next(), parts.next()) else {
        return Err(invalid("bad request line"));
    };
    let (method, path) = (method.to_string(), path.to_string());
    let mut headers = Vec::new();
    loop {
        let line = read_line(reader)?.ok_or_else(|| invalid("connection closed in the head"))?;
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_string(), value.trim().to_string()));
        }
    }
    let mut request = Request {
        method,
        path,
        headers,
        body: Vec::new(),
    };
    let chunked = request
        .header("transfer-encoding")
        .is_some_and(|encoding| encoding.to_ascii_lowercase().contains("chunked"));
    if chunked {
        loop {
            let mut size_line = String::new();
            reader
                .take(64)
                .read_line(&mut size_line)
                .map_err(|_| invalid("bad chunk size"))?;
            let size =
                usize::from_str_radix(size_line.trim().split(';').next().unwrap_or_default(), 16)
                    .map_err(|_| invalid("bad chunk size"))?;
            if request.body.len() + size > MAX_BODY_BYTES {
                return Err(invalid("request body too long"));
            }
            let start = request.body.len();
            request.body.resize(start + size, 0);
            reader.read_exact(&mut request.body[start..])?;
            // The chunk's CRLF, or the trailers' end after the last.
            let mut end = String::new();
            loop {
                end.clear();
                reader.take(MAX_HEAD_BYTES as u64).read_line(&mut end)?;
                if size != 0 || end.trim().is_empty() {
                    break;
                }
            }
            if size == 0 {
                break;
            }
        }
    } else if let Some(length) = request.header("content-length") {
        let length: usize = length.parse().map_err(|_| invalid("bad content length"))?;
        if length > MAX_BODY_BYTES {
            return Err(invalid("request body too long"));
        }
        request.body.resize(length, 0);
        reader.read_exact(&mut request.body)?;
    }
    Ok(Some(request))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(time_ms: i64, attributes: &[(&str, Value)]) -> Value {
        serde_json::json!({
            "timeUnixNano": (time_ms * 1_000_000).to_string(),
            "severityNumber": 9,
            "body": {"stringValue": "event"},
            "attributes": attributes
                .iter()
                .map(|(key, value)| serde_json::json!({"key": key, "value": value}))
                .collect::<Vec<_>>(),
        })
    }

    fn text(value: &str) -> Value {
        serde_json::json!({"stringValue": value})
    }

    fn int(value: i64) -> Value {
        serde_json::json!({"intValue": value.to_string()})
    }

    fn export(records: Vec<Value>) -> Value {
        serde_json::json!({"resourceLogs": [{
            "resource": {"attributes": [{"key": "service.name", "value": text("codex_cli_rs")}]},
            "scopeLogs": [{"scope": {"name": "codex"}, "logRecords": records}],
        }]})
    }

    const CONVERSATION: &str = "019a0000-0000-7000-8000-000000000001";

    /// Made-up events in the shape of Codex's: a request over a WebSocket,
    /// one over HTTP, and the identity fields Codex adds, which are dropped.
    fn session() -> Value {
        let common = |name: &str| {
            vec![
                ("event.name", text(name)),
                ("conversation.id", text(CONVERSATION)),
                ("user.email", text("someone@example.com")),
            ]
        };
        let completed = |ttft: i64, output: &str| {
            let mut attributes = common("codex.sse_event");
            attributes.extend([
                ("event.kind", text("response.completed")),
                ("ttft_ms", int(ttft)),
                ("output_token_count", text(output)),
                ("model_reasoning_effort", text("xhigh")),
            ]);
            attributes
        };
        let mut api_request = common("codex.api_request");
        api_request.push(("duration_ms", text("400")));
        export(vec![
            record(1_000, &common("codex.websocket_request")),
            record(1_500, &{
                let mut created = common("codex.sse_event");
                created.push(("event.kind", text("response.created")));
                created
            }),
            // First output after 2 s, 1000 tokens over the next 10 s.
            record(13_000, &completed(2_000, "1000")),
            // Over HTTP: headers after 400 ms, then 600 ms more.
            record(20_000, &api_request),
            record(25_600, &completed(600, "1000")),
            // No request seen being sent: not counted.
            record(30_000, &completed(900, "1000")),
        ])
    }

    #[test]
    fn test_requests_from_events() {
        let mut telemetry = CodexTelemetry::default();
        telemetry.ingest(&session());
        let summary = telemetry.summary(CONVERSATION).expect("requests");
        assert_eq!(
            summary.latest_first_token,
            Some(Duration::from_millis(1_000))
        );
        assert_eq!(summary.latest_speed, Some(200.));
        assert_eq!(summary.mean_first_token, Some(Duration::from_millis(1_500)));
        assert_eq!(summary.mean_speed, Some(150.));
        assert_eq!(telemetry.effort(CONVERSATION).as_deref(), Some("xhigh"));
        assert_eq!(telemetry.summary("another"), None);
        // Only numbers are kept.
        let kept = format!("{:?}", telemetry);
        assert!(!kept.contains("example.com"), "{kept}");
    }

    #[test]
    fn test_bad_exports_are_skipped() {
        let mut telemetry = CodexTelemetry::default();
        for export in [
            serde_json::json!({}),
            serde_json::json!({"resourceLogs": "x"}),
            serde_json::json!({"resourceLogs": [{"scopeLogs": [{"logRecords": [{"attributes": 1}]}]}]}),
            // Not Codex's, or no session.
            super::tests::export(vec![record(
                1,
                &[
                    ("event.name", text("other.event")),
                    ("conversation.id", text("c")),
                ],
            )]),
            super::tests::export(vec![record(
                1,
                &[("event.name", text("codex.websocket_request"))],
            )]),
            super::tests::export(vec![record(
                1,
                &[
                    ("event.name", text("codex.websocket_request")),
                    ("conversation.id", text("a;b")),
                ],
            )]),
        ] {
            telemetry.ingest(&export);
        }
        assert!(telemetry.conversations.is_empty());
    }

    #[test]
    fn test_overrides_are_valid_toml() {
        let receiver = Receiver::start().unwrap();
        let args = receiver.codex_config_overrides();
        assert_eq!(args[0], "-c");
        assert_eq!(args[2], "-c");
        for arg in [&args[1], &args[3]] {
            assert!(!arg.contains('"') && !arg.contains(' '), "{arg}");
        }
        let (key, value) = args[1].split_once('=').unwrap();
        assert_eq!(key, "otel.exporter");
        let parsed: toml::Table = toml::from_str(&format!("value = {value}")).unwrap();
        let exporter = &parsed["value"]["otlp-http"];
        assert_eq!(
            exporter["endpoint"].as_str(),
            Some(receiver.endpoint().as_str())
        );
        assert_eq!(exporter["protocol"].as_str(), Some("json"));
        assert_eq!(
            exporter["headers"][TOKEN_HEADER].as_str(),
            Some(receiver.token.as_str())
        );
        assert_eq!(receiver.token.len(), 32);
    }

    fn post(port: u16, head: &str, body: &[u8]) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream.write_all(head.as_bytes()).unwrap();
        stream.write_all(body).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    }

    #[test]
    fn test_receiver() {
        let receiver = Receiver::start().unwrap();
        let body = serde_json::to_vec(&session()).unwrap();
        let head = |token: &str, path: &str| {
            format!(
                "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\ncontent-type: application/json\r\n{TOKEN_HEADER}: {token}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
        };
        let wrong = post(receiver.port, &head("wrong", LOGS_PATH), &body);
        assert!(wrong.starts_with("HTTP/1.1 401"), "{wrong}");
        assert_eq!(receiver.summary(CONVERSATION), None);
        let elsewhere = post(receiver.port, &head(&receiver.token, "/v1/traces"), &body);
        assert!(elsewhere.starts_with("HTTP/1.1 404"), "{elsewhere}");

        let ok = post(receiver.port, &head(&receiver.token, LOGS_PATH), &body);
        assert!(ok.starts_with("HTTP/1.1 200 OK"), "{ok}");
        assert!(ok.ends_with("\r\n\r\n{}"), "{ok}");
        assert_eq!(
            receiver.summary(CONVERSATION).unwrap().latest_speed,
            Some(200.)
        );

        // Chunked, and two requests on one connection.
        let mut stream = TcpStream::connect(("127.0.0.1", receiver.port)).unwrap();
        let chunked_head = format!(
            "POST {LOGS_PATH} HTTP/1.1\r\n{TOKEN_HEADER}: {}\r\nTransfer-Encoding: chunked\r\n\r\n",
            receiver.token
        );
        for _ in 0..2 {
            stream.write_all(chunked_head.as_bytes()).unwrap();
            let (first, rest) = body.split_at(body.len() / 2);
            for chunk in [first, rest] {
                write!(stream, "{:x}\r\n", chunk.len()).unwrap();
                stream.write_all(chunk).unwrap();
                stream.write_all(b"\r\n").unwrap();
            }
            stream.write_all(b"0\r\n\r\n").unwrap();
            // The whole response, which ends with its `{}` body.
            let mut response = Vec::new();
            while !response.ends_with(b"{}") {
                let mut byte = [0u8; 1];
                stream.read_exact(&mut byte).unwrap();
                response.push(byte[0]);
            }
            assert!(response.starts_with(b"HTTP/1.1 200 OK"));
        }
    }

    #[test]
    fn test_users_own_telemetry_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let plain = dir.path().join("config.toml");
        std::fs::write(&plain, "model = \"gpt-5\"\n[hooks]\n").unwrap();
        let with_otel = dir.path().join("requirements.toml");
        std::fs::write(&with_otel, "[otel]\nexporter = \"none\"\n").unwrap();
        let missing = dir.path().join("missing.toml");
        assert!(!user_configures_telemetry(&[
            plain.clone(),
            missing.clone()
        ]));
        assert!(user_configures_telemetry(&[plain, with_otel, missing]));
    }
}
