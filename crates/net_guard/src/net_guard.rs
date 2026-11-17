//! SOLO: Solo's network rules.
//!
//! - Offline mode (`network.offline`): Solo's HTTP client only reaches this
//!   computer and the local network ([`OfflineHttpClient`]), and Solo runs no
//!   npm, pip or `go install` that would download something.
//! - Asking before downloading (`downloads`): before Solo downloads a
//!   component by itself (a language server, Node.js, Prettier, a debug
//!   adapter, Copilot, extension updates) it asks once per component, unless
//!   `downloads.mode` is "auto" or "never". The answers are kept in
//!   `downloads.allowed`; an allowed component updates without asking, as
//!   `downloads.check_updates` says.
//!
//! The rules are process-wide, so code that has no `App` (the Node runtime,
//! language server installers) can follow them. Solo's UI feeds the settings
//! in with [`set_policy`] and shows the questions ([`pending`], [`answer`]).

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fmt,
    future::Future,
    net::IpAddr,
    sync::{Arc, LazyLock},
};

use futures::{
    FutureExt as _,
    channel::{mpsc, oneshot},
    future::BoxFuture,
};
use http_client::{AsyncBody, HttpClient, Request, Response, Url, http::HeaderValue};
use parking_lot::Mutex;
use util::i18n::tr;

/// What Solo does when it needs to download a component (`downloads.mode`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DownloadMode {
    /// Ask once per component.
    #[default]
    Ask,
    /// Download without asking (Zed's behavior).
    Auto,
    /// Never download; use what's installed.
    Never,
}

/// The settings the rules follow.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Policy {
    /// `network.offline`
    pub offline: bool,
    /// `downloads.mode`
    pub mode: DownloadMode,
    /// `downloads.allowed`: component id to whether it may be downloaded.
    pub allowed: HashMap<String, bool>,
    /// `downloads.check_updates`: whether allowed components update.
    pub check_updates: bool,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            offline: false,
            mode: DownloadMode::Ask,
            allowed: HashMap::default(),
            check_updates: true,
        }
    }
}

/// Something Solo downloads by itself.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Component {
    /// The key in `downloads.allowed`.
    pub id: String,
    /// The name shown to the user.
    pub name: String,
    /// The domain it's downloaded from, when known.
    pub source: Option<String>,
}

const NPM: &str = "registry.npmjs.org";
const GITHUB: &str = "github.com";

impl Component {
    pub fn new(id: impl Into<String>, name: impl Into<String>, source: Option<&str>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            source: source.map(Into::into),
        }
    }

    /// A language server, by its name (`rust-analyzer`, `vtsls`, ...).
    pub fn language_server(name: &str) -> Self {
        Self::new(name, name, language_server_source(name))
    }

    /// Node.js, which Solo downloads when it finds none on the system.
    pub fn node() -> Self {
        Self::new("node", "Node.js", Some("nodejs.org"))
    }

    /// The Prettier formatter Solo installs for projects that have none.
    pub fn prettier() -> Self {
        Self::new("prettier", "Prettier", Some(NPM))
    }

    /// GitHub Copilot's language server.
    pub fn copilot() -> Self {
        Self::new("copilot", "GitHub Copilot", Some(NPM))
    }

    /// Updates of installed extensions.
    pub fn extension_updates() -> Self {
        Self::new(
            "extension-updates",
            tr("Extension updates", "扩展更新"),
            Some("api.zed.dev"),
        )
    }

    /// A debug adapter, by its adapter name (`CodeLLDB`, `JavaScript`, ...).
    pub fn debug_adapter(adapter_name: &str) -> Self {
        let (id, name, source) = match adapter_name {
            "CodeLLDB" => ("codelldb", "CodeLLDB", GITHUB),
            "JavaScript" => ("js-debug", "js-debug", GITHUB),
            "Delve" | "delve-shim-dap" => ("delve", "Delve", GITHUB),
            "Debugpy" => ("debugpy", "debugpy", "pypi.org"),
            other => return Self::new(other.to_lowercase(), other, None),
        };
        Self::new(id, name, Some(source))
    }

    /// "name (source)", or the name when the source isn't known.
    pub fn label(&self) -> String {
        match &self.source {
            Some(source) => format!("{} ({source})", self.name),
            None => self.name.clone(),
        }
    }
}

fn language_server_source(name: &str) -> Option<&'static str> {
    Some(match name {
        "vtsls"
        | "typescript-language-server"
        | "vscode-css-language-server"
        | "json-language-server"
        | "yaml-language-server"
        | "bash-language-server"
        | "tailwindcss-language-server"
        | "tailwindcss-intellisense-css"
        | "basedpyright"
        | "pyright" => NPM,
        "rust-analyzer"
        | "clangd"
        | "ruff"
        | "ty"
        | "pyrefly"
        | "package-version-server"
        | "eslint" => GITHUB,
        "gopls" => "proxy.golang.org",
        "pylsp" => "pypi.org",
        _ => return None,
    })
}

/// Why a component wasn't downloaded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Blocked {
    /// Offline mode is on.
    Offline,
    /// `downloads.mode` is "never".
    Never,
    /// `downloads.allowed` has it as `false`.
    Denied,
    /// The user answered "Not now" (or closed the question).
    NotNow,
    /// It's installed and `downloads.check_updates` is off.
    NoUpdates,
}

/// The error of a download the rules didn't allow.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownloadBlocked {
    pub name: String,
    pub reason: Blocked,
}

impl fmt::Display for DownloadBlocked {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = &self.name;
        let chinese = util::i18n::is_chinese();
        match (self.reason, chinese) {
            (Blocked::Offline, false) => write!(f, "Offline: {name} not downloaded"),
            (Blocked::Offline, true) => write!(f, "离线：未下载 {name}"),
            (Blocked::Never, false) => write!(f, "{name} not downloaded: downloads are off"),
            (Blocked::Never, true) => write!(f, "未下载 {name}：已关闭下载"),
            (Blocked::Denied, false) => write!(f, "{name} not downloaded: not allowed in settings"),
            (Blocked::Denied, true) => write!(f, "未下载 {name}：设置里未允许"),
            (Blocked::NotNow, false) => write!(f, "{name} not downloaded: you chose Not now"),
            (Blocked::NotNow, true) => write!(f, "未下载 {name}：你选了“暂不”"),
            (Blocked::NoUpdates, false) => write!(f, "{name}: update checks are off"),
            (Blocked::NoUpdates, true) => write!(f, "{name}：已关闭检查更新"),
        }
    }
}

impl std::error::Error for DownloadBlocked {}

/// What the rules say about a download.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Ask,
    Block(Blocked),
}

/// The rules for downloading component `id`.
///
/// `installed`: an earlier version is on disk, so this is an update. A
/// component that's on disk counts as allowed (Solo downloaded it before
/// these rules, or the user put it there).
///
/// `trusted`: the download is for a trusted project. For an untrusted one,
/// Solo asks even when `downloads.mode` is "auto".
pub fn decide(
    policy: &Policy,
    declined: &HashSet<String>,
    id: &str,
    installed: bool,
    trusted: bool,
) -> Decision {
    if policy.offline {
        return Decision::Block(Blocked::Offline);
    }
    if policy.mode == DownloadMode::Never {
        return Decision::Block(Blocked::Never);
    }
    let allowed = policy.allowed.get(id).copied();
    if allowed == Some(false) {
        return Decision::Block(Blocked::Denied);
    }
    if installed && !policy.check_updates {
        return Decision::Block(Blocked::NoUpdates);
    }
    if allowed == Some(true) {
        return Decision::Allow;
    }
    if trusted && (policy.mode == DownloadMode::Auto || installed) {
        return Decision::Allow;
    }
    if declined.contains(id) {
        return Decision::Block(Blocked::NotNow);
    }
    Decision::Ask
}

/// The user's answer to a question.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    /// "Allow (remember)": download now and later without asking.
    Allow,
    /// "Not now": don't download until Solo restarts.
    NotNow,
}

struct Waiting {
    component: Component,
    trusted: bool,
    senders: Vec<oneshot::Sender<bool>>,
}

#[derive(Default)]
struct State {
    policy: Policy,
    declined: HashSet<String>,
    waiting: BTreeMap<String, Waiting>,
    asks_user: bool,
    subscribers: Vec<mpsc::UnboundedSender<()>>,
    /// Allowed in this session, kept when the settings change before the
    /// answer reaches them.
    allowed_now: HashSet<String>,
    to_remember: Vec<String>,
    before_trust: BTreeMap<String, Component>,
}

/// The rules and the questions waiting for an answer. Solo uses one, through
/// the functions of this module; tests make their own.
#[derive(Default)]
pub struct Guard {
    state: Mutex<State>,
}

impl Guard {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_policy(&self, mut policy: Policy) {
        let mut state = self.state.lock();
        for id in &state.allowed_now {
            policy.allowed.entry(id.clone()).or_insert(true);
        }
        if state.policy == policy {
            return;
        }
        state.policy = policy;
        // Settings can settle a question: "auto", offline, or an `allowed`
        // edited by hand.
        let State {
            policy,
            declined,
            waiting,
            before_trust,
            ..
        } = &mut *state;
        waiting.retain(|id, waiting| {
            let allow = match decide(policy, declined, id, false, waiting.trusted) {
                Decision::Ask => return true,
                Decision::Allow => true,
                Decision::Block(_) => false,
            };
            for sender in waiting.senders.drain(..) {
                sender.send(allow).ok();
            }
            false
        });
        before_trust.retain(|id, _| decide(policy, declined, id, false, true) == Decision::Ask);
        Self::notify(&mut state);
    }

    pub fn policy(&self) -> Policy {
        self.state.lock().policy.clone()
    }

    pub fn is_offline(&self) -> bool {
        self.state.lock().policy.offline
    }

    /// Whether installed components may check for and download updates.
    pub fn updates_allowed(&self) -> bool {
        let state = self.state.lock();
        !state.policy.offline
            && state.policy.mode != DownloadMode::Never
            && state.policy.check_updates
    }

    /// Whether a UI answers questions. Without one (tests, headless tools),
    /// questions are answered "Allow", as before these rules.
    pub fn set_asks_user(&self, asks_user: bool) {
        self.state.lock().asks_user = asks_user;
    }

    /// A channel that gets a message whenever a question, an answer or the
    /// rules change.
    pub fn subscribe(&self) -> mpsc::UnboundedReceiver<()> {
        let (tx, rx) = mpsc::unbounded();
        self.state.lock().subscribers.push(tx);
        rx
    }

    fn notify(state: &mut State) {
        state
            .subscribers
            .retain(|subscriber| subscriber.unbounded_send(()).is_ok());
    }

    /// What the rules say about downloading `component` now, without asking.
    pub fn decision(&self, component: &Component, installed: bool, trusted: bool) -> Decision {
        let state = self.state.lock();
        decide(
            &state.policy,
            &state.declined,
            &component.id,
            installed,
            trusted,
        )
    }

    /// Resolves once `component` may be downloaded, asking the user first if
    /// the rules say so, or fails with why it may not.
    pub fn request(
        &self,
        component: Component,
        installed: bool,
        trusted: bool,
    ) -> impl Future<Output = Result<(), DownloadBlocked>> + use<> {
        let name = component.name.clone();
        let blocked = move |reason| DownloadBlocked {
            name: name.clone(),
            reason,
        };
        let mut state = self.state.lock();
        let receiver = match decide(
            &state.policy,
            &state.declined,
            &component.id,
            installed,
            trusted,
        ) {
            Decision::Allow => None,
            Decision::Block(reason) => {
                return futures::future::ready(Err(blocked(reason))).left_future();
            }
            Decision::Ask if !state.asks_user => {
                log::info!("allowing the download of {} (nothing to ask)", component.id);
                None
            }
            Decision::Ask => {
                let (tx, rx) = oneshot::channel();
                log::info!("asking before downloading {}", component.id);
                state
                    .waiting
                    .entry(component.id.clone())
                    .or_insert_with(|| Waiting {
                        component,
                        trusted,
                        senders: Vec::new(),
                    })
                    .senders
                    .push(tx);
                Self::notify(&mut state);
                Some(rx)
            }
        };
        drop(state);
        async move {
            match receiver {
                None => Ok(()),
                Some(receiver) => match receiver.await {
                    Ok(true) => Ok(()),
                    _ => Err(blocked(Blocked::NotNow)),
                },
            }
        }
        .right_future()
    }

    /// The components waiting for an answer.
    pub fn pending(&self) -> Vec<Component> {
        self.state
            .lock()
            .waiting
            .values()
            .map(|waiting| waiting.component.clone())
            .collect()
    }

    /// Answers the questions about `ids` (also ones not asked yet, as the
    /// trust dialog does). Allowed ids go to [`Self::take_to_remember`].
    pub fn answer(&self, ids: &[String], answer: Answer) {
        let mut state = self.state.lock();
        for id in ids {
            let waiting = state.waiting.remove(id);
            match answer {
                Answer::Allow => {
                    state.policy.allowed.insert(id.clone(), true);
                    state.allowed_now.insert(id.clone());
                    state.declined.remove(id);
                    if !state.to_remember.contains(id) {
                        state.to_remember.push(id.clone());
                    }
                }
                Answer::NotNow => {
                    if waiting.is_none() {
                        continue;
                    }
                    state.declined.insert(id.clone());
                }
            }
            state.before_trust.remove(id);
            for sender in waiting.into_iter().flat_map(|waiting| waiting.senders) {
                sender.send(answer == Answer::Allow).ok();
            }
        }
        Self::notify(&mut state);
    }

    /// The ids allowed since the last call, for the UI to keep in
    /// `downloads.allowed`.
    pub fn take_to_remember(&self) -> Vec<String> {
        std::mem::take(&mut self.state.lock().to_remember)
    }

    /// Notes that `component` will be asked for once a project is trusted,
    /// for the trust dialog to list.
    pub fn note_before_trust(&self, component: Component) {
        let mut state = self.state.lock();
        if decide(&state.policy, &state.declined, &component.id, false, true) != Decision::Ask
            || state.before_trust.contains_key(&component.id)
        {
            return;
        }
        state.before_trust.insert(component.id.clone(), component);
        Self::notify(&mut state);
    }

    /// The components listed by [`Self::note_before_trust`].
    pub fn before_trust(&self) -> Vec<Component> {
        self.state.lock().before_trust.values().cloned().collect()
    }
}

static GUARD: LazyLock<Guard> = LazyLock::new(Guard::new);

/// Solo's [`Guard`].
pub fn global() -> &'static Guard {
    &GUARD
}

pub fn set_policy(policy: Policy) {
    GUARD.set_policy(policy)
}

pub fn policy() -> Policy {
    GUARD.policy()
}

/// Whether offline mode is on.
pub fn is_offline() -> bool {
    GUARD.is_offline()
}

/// Whether installed components may check for and download updates.
pub fn updates_allowed() -> bool {
    GUARD.updates_allowed()
}

/// See [`Guard::request`].
pub fn request(
    component: Component,
    installed: bool,
) -> impl Future<Output = Result<(), DownloadBlocked>> {
    GUARD.request(component, installed, true)
}

/// See [`Guard::request`], for a download for a project that may not be
/// trusted.
pub fn request_for_project(
    component: Component,
    installed: bool,
    trusted: bool,
) -> impl Future<Output = Result<(), DownloadBlocked>> {
    GUARD.request(component, installed, trusted)
}

pub fn pending() -> Vec<Component> {
    GUARD.pending()
}

pub fn answer(ids: &[String], answer: Answer) {
    GUARD.answer(ids, answer)
}

pub fn note_before_trust(component: Component) {
    GUARD.note_before_trust(component)
}

pub fn before_trust() -> Vec<Component> {
    GUARD.before_trust()
}

/// Fails when offline mode is on, for a download about to start that the
/// rules didn't see (a subprocess such as `pip` or `go install`).
pub fn ensure_online(name: &str) -> Result<(), DownloadBlocked> {
    if is_offline() {
        Err(DownloadBlocked {
            name: name.to_string(),
            reason: Blocked::Offline,
        })
    } else {
        Ok(())
    }
}

/// Whether npm's `subcommand` reaches the registry.
pub fn npm_subcommand_uses_network(subcommand: &str) -> bool {
    matches!(
        subcommand,
        "install"
            | "i"
            | "add"
            | "ci"
            | "update"
            | "up"
            | "upgrade"
            | "info"
            | "view"
            | "v"
            | "show"
            | "outdated"
            | "exec"
            | "x"
            | "dist-tag"
            | "search"
            | "audit"
    )
}

/// Whether `host` (a name or an IP address) is this computer or the local
/// network: loopback, private and link-local addresses, `localhost`, and names
/// that only resolve locally (no dot, `.local`, `.lan`, `.internal`,
/// `.home.arpa`).
pub fn is_local_host(host: &str) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = host.parse::<IpAddr>() {
        return is_local_ip(ip);
    }
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() {
        return true;
    }
    !host.contains('.')
        || [".localhost", ".local", ".lan", ".internal", ".home.arpa"]
            .iter()
            .any(|suffix| host.ends_with(suffix))
}

fn is_local_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            ip.is_loopback() || ip.is_private() || ip.is_link_local() || ip.is_unspecified()
        }
        IpAddr::V6(ip) => {
            if let Some(ip) = ip.to_ipv4_mapped() {
                return is_local_ip(IpAddr::V4(ip));
            }
            let first = ip.segments()[0];
            ip.is_loopback()
                || ip.is_unspecified()
                // Unique local, fc00::/7
                || first & 0xfe00 == 0xfc00
                // Link-local, fe80::/10
                || first & 0xffc0 == 0xfe80
        }
    }
}

/// Whether a request to `url` may go out in offline mode.
pub fn reachable_offline(url: &Url) -> bool {
    url.host_str().is_none_or(is_local_host)
}

fn uri_reachable_offline(uri: &http_client::Uri) -> bool {
    uri.host().is_none_or(is_local_host)
}

/// An [`HttpClient`] that, in offline mode, only sends requests to this
/// computer and the local network.
pub struct OfflineHttpClient {
    inner: Arc<dyn HttpClient>,
    guard: &'static Guard,
}

impl OfflineHttpClient {
    /// Follows Solo's rules.
    pub fn new(inner: Arc<dyn HttpClient>) -> Self {
        Self::with_guard(inner, global())
    }

    pub fn with_guard(inner: Arc<dyn HttpClient>, guard: &'static Guard) -> Self {
        Self { inner, guard }
    }
}

impl HttpClient for OfflineHttpClient {
    fn user_agent(&self) -> Option<&HeaderValue> {
        self.inner.user_agent()
    }

    fn proxy(&self) -> Option<&Url> {
        self.inner.proxy()
    }

    fn send(
        &self,
        req: Request<AsyncBody>,
    ) -> BoxFuture<'static, anyhow::Result<Response<AsyncBody>>> {
        if self.guard.is_offline() && !uri_reachable_offline(req.uri()) {
            let host = req.uri().host().unwrap_or_default().to_string();
            log::info!("offline: not sending a request to {host}");
            let message = if util::i18n::is_chinese() {
                format!("离线：未连接 {host}")
            } else {
                format!("Offline: {host} not reached")
            };
            return Box::pin(async move { Err(anyhow::anyhow!(message)) });
        }
        self.inner.send(req)
    }
}

#[cfg(test)]
mod tests;
