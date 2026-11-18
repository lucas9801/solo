//! The server's HTTP side: MCP's streamable HTTP transport, as far as a
//! server that only answers requests needs it. Each POST to [`PATH`] carries
//! one JSON-RPC message (or a batch), answered with JSON, or with `202
//! Accepted` when it has only notifications; there's no event stream (GET
//! gets `405`) and no session. Requests go to the app's main thread as
//! [`Call`]s; each is waited for on its own thread.

use std::{
    io::Read as _,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc::{SyncSender, sync_channel},
    },
    time::Duration,
};

use futures::channel::mpsc::UnboundedSender;
use serde_json::Value;

pub const PATH: &str = "/mcp";
const MAX_BODY_BYTES: u64 = 4 * 1024 * 1024;
/// How many requests are answered at once.
const MAX_REQUESTS: usize = 16;
/// Longer than any tool takes.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(600);

/// A request, for the main thread.
pub struct Call {
    /// The bearer token it carries.
    pub token: Option<String>,
    pub messages: Value,
    pub reply: SyncSender<Reply>,
}

#[derive(Debug, PartialEq)]
pub enum Reply {
    Json(Value),
    /// Only notifications: nothing to answer.
    Accepted,
    /// No token, or not a workspace's.
    Unauthorized,
    Forbidden(&'static str),
}

/// Listens on a free port of the loopback interface, in the background, and
/// returns the server's URL.
pub fn start(calls: UnboundedSender<Call>) -> std::io::Result<String> {
    let server = tiny_http::Server::http("127.0.0.1:0").map_err(std::io::Error::other)?;
    let port = server
        .server_addr()
        .to_ip()
        .ok_or_else(|| std::io::Error::other("not an IP address"))?
        .port();
    let busy = Arc::new(AtomicUsize::new(0));
    std::thread::Builder::new()
        .name("solo-ide-tools".to_string())
        .spawn(move || {
            for request in server.incoming_requests() {
                if busy.load(Ordering::SeqCst) >= MAX_REQUESTS {
                    respond(request, 503, None, "");
                    continue;
                }
                busy.fetch_add(1, Ordering::SeqCst);
                let (calls, done) = (calls.clone(), busy.clone());
                let spawned = std::thread::Builder::new()
                    .name("solo-ide-tools-request".to_string())
                    .spawn(move || {
                        handle(request, port, &calls);
                        done.fetch_sub(1, Ordering::SeqCst);
                    });
                if spawned.is_err() {
                    busy.fetch_sub(1, Ordering::SeqCst);
                }
            }
        })?;
    Ok(format!("http://127.0.0.1:{port}{PATH}"))
}

fn header<'a>(request: &'a tiny_http::Request, name: &str) -> Option<&'a str> {
    request
        .headers()
        .iter()
        .find(|header| header.field.as_str().as_str().eq_ignore_ascii_case(name))
        .map(|header| header.value.as_str())
}

/// Whether a request may be answered at all, before its token is looked at:
/// to the server's path, from a program rather than a web page (no
/// `Origin`), addressed to the loopback interface (against DNS rebinding).
fn check(
    method: &str,
    path: &str,
    host: Option<&str>,
    origin: Option<&str>,
    port: u16,
) -> Result<(), u16> {
    if path.split('?').next() != Some(PATH) {
        return Err(404);
    }
    if origin.is_some() {
        return Err(403);
    }
    let host_ok = host.is_some_and(|host| {
        [format!("127.0.0.1:{port}"), format!("localhost:{port}")]
            .iter()
            .any(|allowed| host.eq_ignore_ascii_case(allowed))
    });
    if !host_ok {
        return Err(403);
    }
    if method != "POST" {
        return Err(405);
    }
    Ok(())
}

fn bearer_token(authorization: Option<&str>) -> Option<String> {
    let (scheme, token) = authorization?.trim().split_once(' ')?;
    scheme
        .eq_ignore_ascii_case("bearer")
        .then(|| token.trim().to_string())
        .filter(|token| !token.is_empty())
}

fn handle(mut request: tiny_http::Request, port: u16, calls: &UnboundedSender<Call>) {
    let method = request.method().as_str().to_string();
    if let Err(status) = check(
        &method,
        request.url(),
        header(&request, "host"),
        header(&request, "origin"),
        port,
    ) {
        let allow = (status == 405).then_some(("Allow", "POST"));
        respond(request, status, allow, "");
        return;
    }
    let token = bearer_token(header(&request, "authorization"));
    let mut body = Vec::new();
    if request
        .as_reader()
        .take(MAX_BODY_BYTES + 1)
        .read_to_end(&mut body)
        .is_err()
        || body.len() as u64 > MAX_BODY_BYTES
    {
        respond(request, 413, None, "");
        return;
    }
    let Ok(messages) = serde_json::from_slice::<Value>(&body) else {
        let error = serde_json::json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": "parse error" } });
        respond_json(request, 400, &error);
        return;
    };
    let (reply, answer) = sync_channel(1);
    if calls
        .unbounded_send(Call {
            token,
            messages,
            reply,
        })
        .is_err()
    {
        respond(request, 503, None, "");
        return;
    }
    match answer.recv_timeout(ANSWER_TIMEOUT) {
        Ok(Reply::Json(answer)) => respond_json(request, 200, &answer),
        Ok(Reply::Accepted) => respond(request, 202, None, ""),
        Ok(Reply::Unauthorized) => respond(
            request,
            401,
            Some(("WWW-Authenticate", "Bearer")),
            "This terminal's token isn't one of Solo's workspaces (was Solo restarted? open a new terminal).",
        ),
        Ok(Reply::Forbidden(message)) => respond(request, 403, None, message),
        Err(_) => respond(request, 503, None, ""),
    }
}

fn respond_json(request: tiny_http::Request, status: u16, body: &Value) {
    let body = body.to_string();
    let response = tiny_http::Response::from_string(body)
        .with_status_code(status)
        .with_header(content_type("application/json"));
    request.respond(response).ok();
}

fn respond(request: tiny_http::Request, status: u16, extra: Option<(&str, &str)>, body: &str) {
    let mut response = tiny_http::Response::from_string(body)
        .with_status_code(status)
        .with_header(content_type("text/plain; charset=utf-8"));
    if let Some((name, value)) = extra
        && let Ok(header) = tiny_http::Header::from_bytes(name.as_bytes(), value.as_bytes())
    {
        response.add_header(header);
    }
    request.respond(response).ok();
}

fn content_type(value: &str) -> tiny_http::Header {
    tiny_http::Header::from_bytes(&b"Content-Type"[..], value.as_bytes()).expect("a valid header")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_posts_to_the_path_from_programs_on_this_machine() {
        let host = Some("127.0.0.1:4000");
        assert_eq!(check("POST", "/mcp", host, None, 4000), Ok(()));
        assert_eq!(
            check("POST", "/mcp?x=1", Some("localhost:4000"), None, 4000),
            Ok(())
        );
        assert_eq!(check("GET", "/mcp", host, None, 4000), Err(405));
        assert_eq!(check("POST", "/other", host, None, 4000), Err(404));
        assert_eq!(
            check("POST", "/mcp", host, Some("https://example.com"), 4000),
            Err(403)
        );
        assert_eq!(
            check("POST", "/mcp", Some("evil.example:4000"), None, 4000),
            Err(403)
        );
        assert_eq!(check("POST", "/mcp", None, None, 4000), Err(403));
    }

    #[test]
    fn reads_bearer_tokens() {
        assert_eq!(bearer_token(Some("Bearer abc")), Some("abc".to_string()));
        assert_eq!(bearer_token(Some("bearer  abc ")), Some("abc".to_string()));
        assert_eq!(bearer_token(Some("Basic abc")), None);
        assert_eq!(bearer_token(Some("Bearer ")), None);
        assert_eq!(bearer_token(None), None);
    }

    /// The server end to end, with a stand-in for the main thread.
    #[test]
    #[allow(clippy::disallowed_methods, reason = "a test of a blocking server")]
    fn answers_over_http() {
        use futures::StreamExt as _;
        use std::io::{BufRead as _, BufReader, Write as _};

        let (sender, mut receiver) = futures::channel::mpsc::unbounded::<Call>();
        let url = start(sender).unwrap();
        std::thread::spawn(move || {
            while let Some(call) = futures::executor::block_on(receiver.next()) {
                let reply = match call.token.as_deref() {
                    Some("good") if call.messages.get("id").is_some() => Reply::Json(
                        serde_json::json!({"jsonrpc": "2.0", "id": call.messages["id"], "result": {}}),
                    ),
                    Some("good") => Reply::Accepted,
                    _ => Reply::Unauthorized,
                };
                call.reply.send(reply).ok();
            }
        });
        let address = url
            .strip_prefix("http://")
            .and_then(|rest| rest.strip_suffix(PATH))
            .unwrap()
            .to_string();
        let post = |token: &str, body: &str| -> (String, String) {
            let mut stream = std::net::TcpStream::connect(&address).unwrap();
            write!(
                stream,
                "POST {PATH} HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
            let mut reader = BufReader::new(stream);
            let mut status = String::new();
            reader.read_line(&mut status).unwrap();
            let mut rest = String::new();
            reader.read_to_string(&mut rest).unwrap();
            let body = rest
                .split_once("\r\n\r\n")
                .map(|(_, body)| body.to_string())
                .unwrap_or_default();
            (status.trim().to_string(), body)
        };
        let (status, body) = post("good", r#"{"jsonrpc":"2.0","id":7,"method":"ping"}"#);
        assert_eq!(status, "HTTP/1.1 200 OK");
        assert_eq!(serde_json::from_str::<Value>(&body).unwrap()["id"], 7);
        let (status, _) = post(
            "good",
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        );
        assert!(status.starts_with("HTTP/1.1 202"), "{status}");
        let (status, _) = post("bad", r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#);
        assert!(status.starts_with("HTTP/1.1 401"), "{status}");
        let (status, _) = post("good", "not json");
        assert!(status.starts_with("HTTP/1.1 400"), "{status}");
    }
}
