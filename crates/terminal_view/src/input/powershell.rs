//! A background PowerShell that answers completion requests with
//! `TabExpansion2`, using the same PowerShell as the terminal's shell.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{Context as _, Result, anyhow};
use base64::Engine as _;
use futures::channel::oneshot;
use serde::Deserialize;

use super::completion::{
    CompletionItem, CompletionKind, CompletionSet, byte_to_utf16, utf16_to_byte,
};

const SCRIPT: &str = include_str!("powershell_completer.ps1");
const RESPONSE_PREFIX: &str = "SOLO:";

pub struct PowerShellCompleter {
    program: String,
    requests: Mutex<Option<std::sync::mpsc::Sender<Request>>>,
}

struct Request {
    line: String,
    respond: oneshot::Sender<Result<String>>,
}

#[derive(Deserialize)]
struct Response {
    index: usize,
    length: usize,
    #[serde(default)]
    items: Vec<ResponseItem>,
}

#[derive(Deserialize)]
struct ResponseItem {
    text: String,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    tip: Option<String>,
}

impl PowerShellCompleter {
    /// The completer for `program`, shared by all terminals running it. Its
    /// process starts on the first request.
    pub fn get(program: &str) -> Arc<Self> {
        static COMPLETERS: OnceLock<Mutex<HashMap<String, Arc<PowerShellCompleter>>>> =
            OnceLock::new();
        COMPLETERS
            .get_or_init(Default::default)
            .lock()
            .unwrap()
            .entry(program.to_string())
            .or_insert_with(|| {
                Arc::new(Self {
                    program: program.to_string(),
                    requests: Mutex::new(None),
                })
            })
            .clone()
    }

    pub async fn complete(
        &self,
        input: &str,
        cursor: usize,
        cwd: Option<&Path>,
    ) -> Result<CompletionSet> {
        let line = serde_json::json!({
            "input": input,
            "cursor": byte_to_utf16(input, cursor),
            "cwd": cwd.map(|cwd| cwd.to_string_lossy().into_owned()),
        })
        .to_string();
        let response = self.request(line).await?;
        let response: Response =
            serde_json::from_str(&response).context("parsing PowerShell completions")?;
        let start = utf16_to_byte(input, response.index);
        let end = utf16_to_byte(input, response.index + response.length);
        Ok(CompletionSet {
            start,
            end,
            items: response
                .items
                .into_iter()
                .map(|item| {
                    let kind = match item.kind.as_deref() {
                        Some("Command") => CompletionKind::Command,
                        Some("ParameterName") => CompletionKind::Parameter,
                        Some("ProviderContainer") => CompletionKind::Directory,
                        Some("ProviderItem") => CompletionKind::File,
                        _ => CompletionKind::Other,
                    };
                    let label = item
                        .label
                        .filter(|label| !label.is_empty())
                        .unwrap_or_else(|| item.text.clone());
                    // Tooltips repeat the label for plain items.
                    let detail = item
                        .tip
                        .map(|tip| tip.trim().to_string())
                        .filter(|tip| !tip.is_empty() && *tip != label && *tip != item.text);
                    CompletionItem {
                        text: item.text,
                        label,
                        kind,
                        detail,
                    }
                })
                .collect(),
        })
    }

    async fn request(&self, line: String) -> Result<String> {
        let (respond, response) = oneshot::channel();
        let mut request = Request { line, respond };
        for _ in 0..2 {
            let sender = self.sender();
            match sender.send(request) {
                Ok(()) => return response.await.map_err(|_| anyhow!("completer stopped"))?,
                // The worker died; start a new one and retry once.
                Err(std::sync::mpsc::SendError(returned)) => {
                    *self.requests.lock().unwrap() = None;
                    request = returned;
                }
            }
        }
        Err(anyhow!("PowerShell completer is not running"))
    }

    fn sender(&self) -> std::sync::mpsc::Sender<Request> {
        self.requests
            .lock()
            .unwrap()
            .get_or_insert_with(|| spawn_worker(self.program.clone()))
            .clone()
    }
}

/// Runs the PowerShell process on its own thread, answering one request at a
/// time.
fn spawn_worker(program: String) -> std::sync::mpsc::Sender<Request> {
    let (sender, receiver) = std::sync::mpsc::channel::<Request>();
    std::thread::Builder::new()
        .name("solo-powershell-completer".into())
        .spawn(move || {
            let mut process = match start_process(&program) {
                Ok(process) => process,
                Err(error) => {
                    log::warn!("failed to start PowerShell completer {program}: {error:#}");
                    for request in receiver {
                        request.respond.send(Err(anyhow!("{error:#}"))).ok();
                    }
                    return;
                }
            };
            for request in receiver {
                let result = process.exchange(&request.line);
                let failed = result.is_err();
                request.respond.send(result).ok();
                if failed {
                    // Dropping the receiver makes the next request start a new worker.
                    break;
                }
            }
            process.child.kill().ok();
        })
        .expect("failed to spawn PowerShell completer thread");
    sender
}

struct Process {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Process {
    fn exchange(&mut self, line: &str) -> Result<String> {
        writeln!(self.stdin, "{line}")?;
        self.stdin.flush()?;
        let mut response = String::new();
        loop {
            response.clear();
            if self.stdout.read_line(&mut response)? == 0 {
                return Err(anyhow!("PowerShell completer exited"));
            }
            if let Some(json) = response.trim_end().strip_prefix(RESPONSE_PREFIX) {
                return Ok(json.to_string());
            }
        }
    }
}

fn start_process(program: &str) -> Result<Process> {
    let script: Vec<u8> = SCRIPT.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mut command = Command::new(program);
    command
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-EncodedCommand",
        ])
        .arg(base64::engine::general_purpose::STANDARD.encode(script))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command.spawn()?;
    let stdin = child.stdin.take().context("no stdin")?;
    let stdout = BufReader::new(child.stdout.take().context("no stdout")?);
    Ok(Process {
        child,
        stdin,
        stdout,
    })
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn test_completes_with_real_powershell() {
        let completer =
            PowerShellCompleter::get(r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe");
        let set = futures::executor::block_on(completer.complete("Get-ChildI", 10, None)).unwrap();
        assert_eq!((set.start, set.end), (0, 10));
        assert!(set.items.iter().any(|item| item.text == "Get-ChildItem"));

        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("crates")).unwrap();
        let set =
            futures::executor::block_on(completer.complete("cd cra", 6, Some(dir.path()))).unwrap();
        assert_eq!((set.start, set.end), (3, 6));
        assert_eq!(set.items[0].kind, CompletionKind::Directory);
        assert!(set.items[0].text.contains("crates"));
    }
}
