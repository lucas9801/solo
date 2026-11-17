//! Runs a real WebView2 (skipped when the runtime isn't installed) and talks
//! to its debugging port with a plain DevTools Protocol client.

use std::{
    io::{Read as _, Write as _},
    net::{Ipv4Addr, SocketAddr, TcpStream, UdpSocket},
    time::{Duration, Instant},
};

use futures::channel::mpsc;
use windows::Win32::{
    System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx},
    UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, DispatchMessageW, MSG, PM_REMOVE, PeekMessageW,
        TranslateMessage, WS_OVERLAPPEDWINDOW,
    },
};

use super::*;

/// Pumps window messages until `done` says so, for at most `timeout`.
fn pump_until(timeout: Duration, mut done: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if done() {
            return true;
        }
        unsafe {
            let mut message = MSG::default();
            while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    done()
}

fn http_get(port: u16, path: &str) -> std::io::Result<String> {
    let mut stream = TcpStream::connect(("127.0.0.1", port))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"
    )?;
    // The server keeps the connection open: read the headers, then the body's length.
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte)?;
        head.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&head).to_ascii_lowercase();
    let length = head
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .and_then(|length| length.trim().parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; length];
    stream.read_exact(&mut body)?;
    Ok(String::from_utf8_lossy(&body).into_owned())
}

/// The smallest WebSocket client the DevTools Protocol needs: text frames.
struct WebSocket(TcpStream);

impl WebSocket {
    fn connect(url: &str) -> std::io::Result<Self> {
        let rest = url.strip_prefix("ws://").expect("a ws:// url");
        let (host, path) = rest.split_once('/').expect("a path");
        let mut stream = TcpStream::connect(host)?;
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        write!(
            stream,
            "GET /{path} HTTP/1.1\r\nHost: {host}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n"
        )?;
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte)?;
            head.push(byte[0]);
        }
        let head = String::from_utf8_lossy(&head);
        assert!(head.starts_with("HTTP/1.1 101"), "handshake: {head}");
        Ok(Self(stream))
    }

    fn send(&mut self, text: &str) -> std::io::Result<()> {
        let payload = text.as_bytes();
        let mut frame = vec![0x81u8];
        match payload.len() {
            len if len < 126 => frame.push(0x80 | len as u8),
            len if len < 65536 => {
                frame.push(0x80 | 126);
                frame.extend_from_slice(&(len as u16).to_be_bytes());
            }
            len => {
                frame.push(0x80 | 127);
                frame.extend_from_slice(&(len as u64).to_be_bytes());
            }
        }
        let mask = [1u8, 2, 3, 4];
        frame.extend_from_slice(&mask);
        frame.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
        self.0.write_all(&frame)
    }

    fn receive(&mut self) -> std::io::Result<String> {
        let mut head = [0u8; 2];
        self.0.read_exact(&mut head)?;
        let mut len = (head[1] & 0x7f) as u64;
        if len == 126 {
            let mut bytes = [0u8; 2];
            self.0.read_exact(&mut bytes)?;
            len = u16::from_be_bytes(bytes) as u64;
        } else if len == 127 {
            let mut bytes = [0u8; 8];
            self.0.read_exact(&mut bytes)?;
            len = u64::from_be_bytes(bytes);
        }
        let mut payload = vec![0u8; len as usize];
        self.0.read_exact(&mut payload)?;
        Ok(String::from_utf8_lossy(&payload).into_owned())
    }

    /// Sends a command and returns its response, skipping events.
    fn call(&mut self, id: u64, method: &str, params: serde_json::Value) -> serde_json::Value {
        let message = serde_json::json!({ "id": id, "method": method, "params": params });
        self.send(&message.to_string()).unwrap();
        loop {
            let reply: serde_json::Value = serde_json::from_str(&self.receive().unwrap()).unwrap();
            if reply["id"] == id {
                return reply;
            }
        }
    }
}

/// This computer's address on its network, if it has one: what another
/// computer would connect to. No packet is sent.
fn lan_address() -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?;
    match socket.local_addr().ok()? {
        SocketAddr::V4(address) if !address.ip().is_loopback() => Some(*address.ip()),
        _ => None,
    }
}

fn start_host(
    config: &EnvironmentConfig,
) -> Option<(HWND, WebViewHost, mpsc::UnboundedReceiver<HostEvent>)> {
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().ok();
    }
    let parent = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("STATIC"),
            w!("browser test"),
            WS_OVERLAPPEDWINDOW,
            0,
            0,
            480,
            360,
            None,
            None,
            None,
            None,
        )
    }
    .ok()?;
    let (tx, mut rx) = mpsc::unbounded();
    let host = WebViewHost::new(
        parent,
        config,
        Some("about:blank".to_string()),
        None,
        tx,
        Default::default(),
    )
    .ok()?;
    // A web view with no size never loads anything.
    host.place(Some(Placement {
        rect: RECT {
            left: 0,
            top: 0,
            right: 400,
            bottom: 300,
        },
        holes: Vec::new(),
    }));
    let mut ready = false;
    let mut loaded = false;
    let mut failed = None;
    pump_until(Duration::from_secs(60), || {
        while let Ok(event) = rx.try_recv() {
            match event {
                HostEvent::Ready => ready = true,
                HostEvent::Loading(false) => loaded = true,
                HostEvent::Failed(error) => failed = Some(error),
                _ => {}
            }
        }
        loaded || failed.is_some()
    });
    assert!(failed.is_none(), "web view failed: {failed:?}");
    assert!(ready && loaded, "web view did not start");
    Some((parent, host, rx))
}

/// With the debugging port on: it answers on 127.0.0.1 only, and a plain
/// DevTools client can read and drive the page Solo shows.
#[test]
fn debugging_port_is_local_and_drives_the_page() {
    if find_runtime().is_err() {
        eprintln!("no WebView2 runtime: skipped");
        return;
    }
    let data = tempfile::tempdir().unwrap();
    let port = crate::agent_control::free_local_port().unwrap();
    let config = EnvironmentConfig {
        user_data_dir: data.path().to_path_buf(),
        remote_debugging_port: Some(port),
    };
    let Some((parent, host, mut events)) = start_host(&config) else {
        panic!("could not create the web view");
    };

    let mut version = String::new();
    pump_until(Duration::from_secs(20), || {
        version = http_get(port, "/json/version").unwrap_or_default();
        !version.is_empty()
    });
    let version: serde_json::Value = serde_json::from_str(&version).expect("/json/version");
    assert!(
        version["webSocketDebuggerUrl"]
            .as_str()
            .is_some_and(|url| url.starts_with(&format!("ws://127.0.0.1:{port}/"))),
        "{version}"
    );

    if let Some(address) = lan_address() {
        let reached =
            TcpStream::connect_timeout(&SocketAddr::from((address, port)), Duration::from_secs(2));
        assert!(reached.is_err(), "the port answers on {address}");
    }

    // The page Solo shows is a target; drive it.
    let list: serde_json::Value =
        serde_json::from_str(&http_get(port, "/json/list").unwrap()).unwrap();
    let page = list
        .as_array()
        .unwrap()
        .iter()
        .find(|target| target["type"] == "page")
        .expect("a page target")
        .clone();
    let page_url = page["webSocketDebuggerUrl"].as_str().unwrap().to_string();
    let page_url_again = page_url.clone();
    let worker = std::thread::spawn(move || {
        let mut socket = WebSocket::connect(&page_url).unwrap();
        let html = "<button id=b onclick=\"this.textContent='clicked'\">go</button><input id=i>";
        socket.call(
            1,
            "Runtime.evaluate",
            serde_json::json!({ "expression": format!("document.body.innerHTML = {html:?}") }),
        );
        socket.call(
            2,
            "Runtime.evaluate",
            serde_json::json!({ "expression": "document.getElementById('b').click(); document.getElementById('i').value = 'typed'; 1" }),
        );
        let result = socket.call(
            3,
            "Runtime.evaluate",
            serde_json::json!({ "expression": "document.getElementById('b').textContent + ' ' + document.getElementById('i').value", "returnByValue": true }),
        );
        let shot = socket.call(
            4,
            "Page.captureScreenshot",
            serde_json::json!({ "format": "png" }),
        );
        (
            result["result"]["result"]["value"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            shot["result"]["data"].as_str().map(str::len).unwrap_or(0),
        )
    });
    // The page's process runs on its own; this thread keeps the host's messages moving.
    pump_until(Duration::from_secs(30), || worker.is_finished());
    let (text, screenshot_len) = worker.join().unwrap();
    assert_eq!(text, "clicked typed");
    assert!(screenshot_len > 100);

    // A window the page opens (window.open, target=_blank) asks the host for
    // it; a second web view takes it over, as a new tab does. (A DevTools
    // client's Target.createTarget does not ask: WebView2 opens a window of
    // its own for it.)
    let worker = std::thread::spawn(move || {
        let mut socket = WebSocket::connect(&page_url_again).unwrap();
        socket.call(
            9,
            "Runtime.evaluate",
            serde_json::json!({ "expression": "window.open('about:blank#new') ? 1 : 0", "userGesture": true, "returnByValue": true }),
        )
    });
    let mut request = None;
    pump_until(Duration::from_secs(30), || {
        while let Ok(event) = events.try_recv() {
            if let HostEvent::NewWindow(new_window) = event {
                request = Some(new_window);
            }
        }
        request.is_some() || worker.is_finished()
    });
    let request = request.expect("the host was asked for a window");
    let (tx, mut second_events) = mpsc::unbounded();
    let second = WebViewHost::new(
        parent,
        &config,
        Some(request.url.clone()),
        Some(request),
        tx,
        Default::default(),
    )
    .unwrap();
    second.place(Some(Placement {
        rect: RECT {
            left: 0,
            top: 0,
            right: 400,
            bottom: 300,
        },
        holes: Vec::new(),
    }));
    let mut second_ready = false;
    pump_until(Duration::from_secs(30), || {
        while let Ok(event) = second_events.try_recv() {
            if let HostEvent::Ready = event {
                second_ready = true;
            }
        }
        second_ready && worker.is_finished()
    });
    let opened = worker.join().unwrap();
    assert_eq!(
        opened["result"]["result"]["value"], 1,
        "window.open: {opened}"
    );
    let pages: serde_json::Value =
        serde_json::from_str(&http_get(port, "/json/list").unwrap()).unwrap();
    assert!(
        pages
            .as_array()
            .unwrap()
            .iter()
            .any(|target| target["url"] == "about:blank#new"),
        "{pages}"
    );
    drop(second);

    drop(host);
    unsafe { DestroyWindow(parent).ok() };
}

/// Without the setting there is no port: nothing listens for DevTools.
#[test]
fn no_debugging_port_by_default() {
    if find_runtime().is_err() {
        return;
    }
    let data = tempfile::tempdir().unwrap();
    let config = EnvironmentConfig {
        user_data_dir: data.path().to_path_buf(),
        remote_debugging_port: None,
    };
    let Some((parent, host, _events)) = start_host(&config) else {
        panic!("could not create the web view");
    };
    assert!(
        !data
            .path()
            .join("EBWebView")
            .join("DevToolsActivePort")
            .exists()
    );
    drop(host);
    unsafe { DestroyWindow(parent).ok() };
}
