//! A background PowerShell, the same one as the terminal's shell, that answers
//! completion requests with `TabExpansion2` and runs the commands of spec
//! generators (e.g. listing git branches).

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow};
use base64::Engine as _;
use futures::channel::oneshot;
use serde::Deserialize;

use super::completion::{
    CompletionItem, CompletionKind, CompletionSet, byte_to_utf16, utf16_to_byte,
};

const SCRIPT: &str = include_str!("powershell_completer.ps1");
const RESPONSE_PREFIX: &str = "SOLO:";
/// Completion requests that take longer than this get the process restarted.
const COMPLETION_TIMEOUT: Duration = Duration::from_secs(3);
/// Generators may call slow tools (cloud CLIs), so they get longer.
const EXEC_TIMEOUT: Duration = Duration::from_secs(5);

pub struct PowerShellCompleter {
    program: String,
    timeout: Duration,
    requests: Mutex<Option<std::sync::mpsc::Sender<Request>>>,
}

struct Request {
    line: String,
    respond: oneshot::Sender<Result<String>>,
}

#[derive(Deserialize)]
struct Response {
    #[serde(default)]
    index: usize,
    #[serde(default)]
    length: usize,
    #[serde(default)]
    items: Vec<ResponseItem>,
    #[serde(default)]
    output: Option<String>,
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
        Self::shared(program, "complete", COMPLETION_TIMEOUT)
    }

    /// A separate process for running generator commands, so a slow one
    /// doesn't hold up completions.
    pub fn get_for_generators(program: &str) -> Arc<Self> {
        Self::shared(program, "exec", EXEC_TIMEOUT)
    }

    fn shared(program: &str, role: &str, timeout: Duration) -> Arc<Self> {
        static COMPLETERS: OnceLock<Mutex<HashMap<(String, String), Arc<PowerShellCompleter>>>> =
            OnceLock::new();
        COMPLETERS
            .get_or_init(Default::default)
            .lock()
            .unwrap()
            .entry((program.to_string(), role.to_string()))
            .or_insert_with(|| {
                Arc::new(Self {
                    program: program.to_string(),
                    timeout,
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

    /// Runs `command` in `cwd` and returns its output.
    pub async fn exec(&self, command: &str, cwd: Option<&Path>) -> Result<String> {
        let line = serde_json::json!({
            "exec": command,
            "cwd": cwd.map(|cwd| cwd.to_string_lossy().into_owned()),
        })
        .to_string();
        let response = self.request(line).await?;
        let response: Response =
            serde_json::from_str(&response).context("parsing PowerShell output")?;
        Ok(response.output.unwrap_or_default())
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
            .get_or_insert_with(|| spawn_worker(self.program.clone(), self.timeout))
            .clone()
    }
}

/// Runs the PowerShell process on its own thread, answering one request at a
/// time.
fn spawn_worker(program: String, timeout: Duration) -> std::sync::mpsc::Sender<Request> {
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
                let result = process.exchange(&request.line, timeout);
                let failed = result.is_err();
                request.respond.send(result).ok();
                if failed {
                    // Dropping the receiver makes the next request start a new worker.
                    break;
                }
            }
            process.kill();
        })
        .expect("failed to spawn PowerShell completer thread");
    sender
}

struct Process {
    child: Arc<Mutex<Child>>,
    /// When the request in flight times out; a watchdog kills the process then.
    deadline: Arc<Mutex<Option<Instant>>>,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Process {
    fn exchange(&mut self, line: &str, timeout: Duration) -> Result<String> {
        writeln!(self.stdin, "{line}")?;
        self.stdin.flush()?;
        *self.deadline.lock().unwrap() = Some(Instant::now() + timeout);
        let result = self.read_response();
        *self.deadline.lock().unwrap() = None;
        result
    }

    fn read_response(&mut self) -> Result<String> {
        let mut response = String::new();
        loop {
            response.clear();
            if self.stdout.read_line(&mut response)? == 0 {
                return Err(anyhow!("PowerShell completer exited or timed out"));
            }
            if let Some(json) = response.trim_end().strip_prefix(RESPONSE_PREFIX) {
                return Ok(json.to_string());
            }
        }
    }

    fn kill(&self) {
        self.child.lock().unwrap().kill().ok();
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
    hide_window(&mut command);
    let mut child = command.spawn()?;
    let stdin = child.stdin.take().context("no stdin")?;
    let stdout = BufReader::new(child.stdout.take().context("no stdout")?);
    let child = Arc::new(Mutex::new(child));
    let deadline = Arc::new(Mutex::new(None::<Instant>));
    spawn_watchdog(child.clone(), deadline.clone());
    Ok(Process {
        child,
        deadline,
        stdin,
        stdout,
    })
}

/// Kills the process once a request runs past its deadline, which unblocks
/// the worker waiting for the response.
fn spawn_watchdog(child: Arc<Mutex<Child>>, deadline: Arc<Mutex<Option<Instant>>>) {
    std::thread::Builder::new()
        .name("solo-powershell-watchdog".into())
        .spawn(move || {
            loop {
                std::thread::sleep(Duration::from_millis(100));
                let mut child = child.lock().unwrap();
                if !matches!(child.try_wait(), Ok(None)) {
                    return;
                }
                if deadline
                    .lock()
                    .unwrap()
                    .is_some_and(|deadline| Instant::now() > deadline)
                {
                    log::warn!("PowerShell completer timed out; restarting it");
                    child.kill().ok();
                    return;
                }
            }
        })
        .ok();
}

pub fn hide_window(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = command;
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    const POWERSHELL: &str = r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe";

    #[test]
    fn test_completes_with_real_powershell() {
        let completer = PowerShellCompleter::get(POWERSHELL);
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

    #[test]
    fn test_exec_with_real_powershell() {
        let completer = PowerShellCompleter::get_for_generators(POWERSHELL);
        let output =
            futures::executor::block_on(completer.exec("Write-Output 'a'; Write-Output 'b'", None))
                .unwrap();
        assert_eq!(output.lines().collect::<Vec<_>>(), vec!["a", "b"]);
    }
}
