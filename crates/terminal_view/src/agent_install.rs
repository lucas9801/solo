//! SOLO: which agent CLIs are installed, and a window to install the others.
//!
//! One list of the agents Solo can follow ([`CATALOG`]) serves the terminal
//! column's empty state, its "+" menu, the new worktree dialog and the
//! "Install Agents" window. [`AgentInventory`] looks for them in the
//! background; a look after an install reads the `PATH` from the registry
//! first, because the installer changed it where Solo's own environment can't
//! see.

use std::{
    cell::{Cell, RefCell},
    ffi::{OsStr, OsString},
    io::Read as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    rc::Rc,
    sync::mpsc,
    time::Duration,
};

use gpui::{
    App, AppContext as _, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable,
    Global, Render, SharedString, Task, WeakEntity, Window, actions,
};
use settings::Settings as _;
use task::RevealStrategy;
use terminal::terminal_settings::TerminalSettings;
use ui::{ButtonLike, ButtonStyle, TintColor, prelude::*};
use util::{
    ResultExt as _,
    i18n::{is_chinese, tr},
};
use workspace::{ModalView, Workspace};

use crate::{
    agent_tab::render_agent_mark, command_blocks::terminal_views, default_working_directory,
    terminal_panel::TerminalPanel,
};

actions!(
    terminal_panel,
    [
        /// Lists the agents Solo can follow, to open the ones that are
        /// installed and install the others.
        InstallAgents
    ]
);

/// An agent CLI Solo can follow.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct AgentSpec {
    pub name: &'static str,
    pub command: &'static str,
    /// What to type in a Windows terminal to install it, from the agent's
    /// own documentation (`docs_url`).
    pub install_command: &'static str,
    pub docs_url: &'static str,
    /// Installed with npm, so Node.js has to be there first.
    pub needs_npm: bool,
}

/// How many of [`CATALOG`]'s first agents are the common ones; the rest are
/// under "More".
pub(crate) const COMMON_COUNT: usize = 6;

/// How many installed agents the terminal column's empty state lists.
pub(crate) const EMPTY_STATE_LIMIT: usize = 4;

const NODE_DOWNLOAD_URL: &str = "https://nodejs.org/en/download";

/// How long `--version` may take before the version stays unknown.
const VERSION_TIMEOUT: Duration = Duration::from_secs(2);

/// The agents Solo reports the status of, the common ones first.
pub(crate) const CATALOG: [AgentSpec; 11] = [
    AgentSpec {
        name: "Claude Code",
        command: "claude",
        install_command: r#"powershell -ExecutionPolicy Bypass -c "irm https://claude.ai/install.ps1 | iex""#,
        docs_url: "https://code.claude.com/docs/en/setup",
        needs_npm: false,
    },
    AgentSpec {
        name: "Codex",
        command: "codex",
        install_command: r#"powershell -ExecutionPolicy ByPass -c "irm https://chatgpt.com/codex/install.ps1 | iex""#,
        docs_url: "https://github.com/openai/codex",
        needs_npm: false,
    },
    AgentSpec {
        name: "Gemini CLI",
        command: "gemini",
        install_command: "npm install -g @google/gemini-cli",
        docs_url: "https://github.com/google-gemini/gemini-cli",
        needs_npm: true,
    },
    AgentSpec {
        name: "Copilot CLI",
        command: "copilot",
        install_command: "winget install GitHub.Copilot",
        docs_url: "https://github.com/github/copilot-cli",
        needs_npm: false,
    },
    AgentSpec {
        name: "OpenCode",
        command: "opencode",
        install_command: "npm i -g @opencode/cli@latest",
        docs_url: "https://github.com/anomalyco/opencode",
        needs_npm: true,
    },
    AgentSpec {
        name: "Qwen Code",
        command: "qwen",
        install_command: r#"powershell -ExecutionPolicy Bypass -c "irm https://qwen-code-assets.oss-cn-hangzhou.aliyuncs.com/installation/install-qwen-standalone.ps1 | iex""#,
        docs_url: "https://github.com/QwenLM/qwen-code",
        needs_npm: false,
    },
    AgentSpec {
        name: "Pi",
        command: "pi",
        install_command: r#"powershell -ExecutionPolicy Bypass -c "irm https://pi.dev/install.ps1 | iex""#,
        docs_url: "https://github.com/badlogic/pi-mono/tree/main/packages/coding-agent",
        needs_npm: false,
    },
    AgentSpec {
        name: "oh-my-pi",
        command: "omp",
        install_command: r#"powershell -ExecutionPolicy Bypass -c "irm https://omp.sh/install.ps1 | iex""#,
        docs_url: "https://github.com/can1357/oh-my-pi",
        needs_npm: false,
    },
    AgentSpec {
        name: "Kilo Code CLI",
        command: "kilo",
        install_command: "npm install -g @kilocode/cli",
        docs_url: "https://github.com/Kilo-Org/kilocode",
        needs_npm: true,
    },
    AgentSpec {
        name: "Kimi Code CLI",
        command: "kimi",
        install_command: r#"powershell -ExecutionPolicy Bypass -c "irm https://code.kimi.com/kimi-code/install.ps1 | iex""#,
        docs_url: "https://github.com/MoonshotAI/kimi-cli",
        needs_npm: false,
    },
    AgentSpec {
        name: "MastraCode",
        command: "mastracode",
        install_command: "npm install -g mastracode",
        docs_url: "https://code.mastra.ai/",
        needs_npm: true,
    },
];

/// An agent found on the `PATH`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InstalledAgent {
    pub spec: &'static AgentSpec,
    /// What `--version` said, if it did in time.
    pub version: Option<String>,
}

/// What a look at the machine found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Scan {
    /// In [`CATALOG`]'s order.
    pub installed: Vec<InstalledAgent>,
    pub npm_found: bool,
}

/// Looks for the agents (and npm) with `locate`, and asks each one found for
/// its version with `version_of`, all at once.
pub(crate) fn scan_with(
    locate: impl Fn(&str) -> Option<PathBuf> + Sync,
    version_of: impl Fn(&Path) -> Option<String> + Sync,
) -> Scan {
    let found: Vec<(&'static AgentSpec, PathBuf)> = CATALOG
        .iter()
        .filter_map(|spec| Some((spec, locate(spec.command)?)))
        .collect();
    let version_of = &version_of;
    let installed = std::thread::scope(|scope| {
        let probes: Vec<_> = found
            .iter()
            .map(|(spec, path)| {
                scope.spawn(move || InstalledAgent {
                    spec,
                    version: version_of(path),
                })
            })
            .collect();
        probes
            .into_iter()
            .filter_map(|probe| probe.join().ok())
            .collect()
    });
    Scan {
        installed,
        npm_found: locate("npm").is_some(),
    }
}

/// Where `command` is on `search_path` (the process's own `PATH` without one).
pub(crate) fn locate(command: &str, search_path: Option<&OsStr>) -> Option<PathBuf> {
    match search_path {
        None => which::which(command).ok(),
        Some(search_path) => which::which_in(
            command,
            Some(search_path),
            std::env::current_dir().unwrap_or_default(),
        )
        .ok(),
    }
}

/// The `PATH` among `variables`.
pub(crate) fn path_among(variables: &[(String, String)]) -> Option<OsString> {
    variables
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("PATH"))
        .map(|(_, value)| OsString::from(value))
}

/// The `PATH` to look in: the process's own with what the registry adds
/// (Solo started from Explorer can lack the user's `Path`; an install changes
/// the registry's). Development builds can pin it with
/// `SOLO_AGENT_SEARCH_PATH`, so a test never sees (or runs) the agents of the
/// machine it's on.
fn search_path() -> Option<OsString> {
    if let Some(pinned) = std::env::var_os("SOLO_AGENT_SEARCH_PATH") {
        return Some(pinned);
    }
    path_among(&util::registry_environment::current_fresh_variables())
}

fn scan_machine(with_versions: bool) -> Scan {
    let search_path = search_path();
    scan_with(
        |command| locate(command, search_path.as_deref()),
        |program| {
            with_versions
                .then(|| read_version(program, VERSION_TIMEOUT))
                .flatten()
        },
    )
}

/// The version `program --version` prints, if it prints one within
/// `timeout`. A program still running then is killed.
#[allow(
    clippy::disallowed_methods,
    reason = "runs on a background thread and waits for the program itself, to kill it after the timeout"
)]
pub(crate) fn read_version(program: &Path, timeout: Duration) -> Option<String> {
    let mut command = Command::new(program);
    command
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        command.creation_flags(0x0800_0000);
    }
    let mut child = command.spawn().log_err()?;
    let stdout = child.stdout.take()?;
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let mut output = Vec::new();
        stdout.take(8192).read_to_end(&mut output).ok();
        sender
            .send(String::from_utf8_lossy(&output).into_owned())
            .ok();
    });
    match receiver.recv_timeout(timeout) {
        Ok(output) => {
            child.wait().ok();
            parse_version(&output)
        }
        Err(_) => {
            // A `.cmd` agent runs node in a child process of its own: ending
            // `cmd.exe` alone would leave it running. Only the tree of the
            // process started here is ended.
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt as _;
                Command::new("taskkill")
                    .args(["/T", "/F", "/PID", &child.id().to_string()])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .creation_flags(0x0800_0000)
                    .status()
                    .log_err();
            }
            child.kill().ok();
            child.wait().ok();
            None
        }
    }
}

/// The first version number in what `--version` printed: "2.4.6 (Claude
/// Code)", "codex-cli 0.171.0" and "v1.2.3-beta.1" give "2.4.6", "0.171.0"
/// and "1.2.3-beta.1".
pub(crate) fn parse_version(output: &str) -> Option<String> {
    output
        .split_whitespace()
        .map(|word| {
            word.trim_matches(|c: char| !c.is_ascii_alphanumeric())
                .trim_start_matches(['v', 'V'])
        })
        .find(|word| {
            word.starts_with(|c: char| c.is_ascii_digit())
                && word.contains('.')
                && word
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'))
        })
        .map(str::to_string)
}

/// Which agents are installed, shared by everything that lists them.
pub(crate) struct AgentInventory {
    scan: Scan,
    /// Whether a look has finished.
    scanned: bool,
    checking: bool,
    /// Asked to look again while a look was running: it runs once more when
    /// that one ends (an install may have finished meanwhile), asking for
    /// versions if any asker did.
    look_again: Option<bool>,
    /// Takes whether to read the versions too.
    scanner: fn(bool) -> Scan,
    looking: Option<Task<()>>,
}

struct GlobalAgentInventory(Entity<AgentInventory>);

impl Global for GlobalAgentInventory {}

/// Tests never look up or run real agents.
fn default_scanner() -> fn(bool) -> Scan {
    if cfg!(test) {
        |_| Scan::default()
    } else {
        scan_machine
    }
}

impl AgentInventory {
    /// The inventory. It looks only when asked to ([`Self::look`]), so a
    /// remote project never makes Solo look at this machine.
    pub(crate) fn global(cx: &mut App) -> Entity<Self> {
        if let Some(inventory) = cx.try_global::<GlobalAgentInventory>() {
            return inventory.0.clone();
        }
        let inventory = cx.new(|_| Self {
            scan: Scan::default(),
            scanned: false,
            checking: false,
            look_again: None,
            scanner: default_scanner(),
            looking: None,
        });
        cx.set_global(GlobalAgentInventory(inventory.clone()));
        inventory
    }

    pub(crate) fn installed(&self) -> &[InstalledAgent] {
        &self.scan.installed
    }

    pub(crate) fn is_installed(&self, spec: &AgentSpec) -> Option<&InstalledAgent> {
        self.scan
            .installed
            .iter()
            .find(|installed| installed.spec == spec)
    }

    pub(crate) fn npm_found(&self) -> bool {
        self.scan.npm_found
    }

    /// Whether the first look has finished.
    pub(crate) fn scanned(&self) -> bool {
        self.scanned
    }

    pub(crate) fn checking(&self) -> bool {
        self.checking
    }

    /// The first look, if none has started.
    pub(crate) fn look_once(&mut self, cx: &mut Context<Self>) {
        if !self.scanned && !self.checking {
            self.look(false, cx);
        }
    }

    /// Looks again, reading each agent's version too when `with_versions`
    /// (the versions are only shown in the install window). A look asked for
    /// while one runs waits for it and then runs once.
    pub(crate) fn look(&mut self, with_versions: bool, cx: &mut Context<Self>) {
        if self.checking {
            let with_versions = with_versions || self.look_again.unwrap_or(false);
            self.look_again = Some(with_versions);
            return;
        }
        self.checking = true;
        let scanner = self.scanner;
        self.looking = Some(cx.spawn(async move |inventory, cx| {
            let mut scan = cx
                .background_spawn(async move { scanner(with_versions) })
                .await;
            inventory
                .update(cx, |inventory, cx| {
                    if !with_versions {
                        // Versions already read stay.
                        for agent in &mut scan.installed {
                            agent.version = inventory
                                .is_installed(agent.spec)
                                .and_then(|known| known.version.clone());
                        }
                    }
                    inventory.scan = scan;
                    inventory.scanned = true;
                    inventory.checking = false;
                    inventory.looking = None;
                    if let Some(with_versions) = inventory.look_again.take() {
                        inventory.look(with_versions, cx);
                    }
                    cx.notify();
                })
                .log_err();
        }));
        cx.notify();
    }

    #[cfg(test)]
    pub(crate) fn set_scanner(&mut self, scanner: fn(bool) -> Scan) {
        self.scanner = scanner;
    }

    #[cfg(test)]
    pub(crate) fn set_installed_for_test(&mut self, commands: &[&str], npm_found: bool) {
        self.scan = Scan {
            installed: CATALOG
                .iter()
                .filter(|spec| commands.contains(&spec.command))
                .map(|spec| InstalledAgent {
                    spec,
                    version: None,
                })
                .collect(),
            npm_found,
        };
        self.scanned = true;
    }
}

/// The installed agents the workspace's column may offer: none in a remote
/// project, whose agents would be on the remote's `PATH`.
pub(crate) fn listed_agents(
    workspace: &WeakEntity<Workspace>,
    cx: &mut App,
) -> Vec<InstalledAgent> {
    if !is_local(workspace, cx) {
        return Vec::new();
    }
    AgentInventory::global(cx).read(cx).installed().to_vec()
}

/// What the "+" menu lists: the installed agents and whether it offers to
/// install more. Neither in a remote project.
pub(crate) fn install_menu_entries(
    workspace: Option<&WeakEntity<Workspace>>,
    cx: &mut App,
) -> (Vec<InstalledAgent>, bool) {
    match workspace {
        Some(workspace) if is_local(workspace, cx) => (listed_agents(workspace, cx), true),
        _ => (Vec::new(), false),
    }
}

pub(crate) fn is_local(workspace: &WeakEntity<Workspace>, cx: &App) -> bool {
    workspace
        .upgrade()
        .is_some_and(|workspace| workspace.read(cx).project().read(cx).is_local())
}

pub(crate) fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _window, cx| {
        // Without a handler, the command palette doesn't list the action:
        // a remote project's agents aren't on this machine.
        if !workspace.project().read(cx).is_local() {
            return;
        }
        workspace.register_action(|workspace, _: &InstallAgents, window, cx| {
            InstallAgentsModal::toggle(workspace, window, cx);
        });
    })
    .detach();
}

/// Opens the "Install Agents" window.
pub(crate) fn show_install_window(
    workspace: &WeakEntity<Workspace>,
    window: &mut Window,
    cx: &mut App,
) {
    workspace
        .update(cx, |workspace, cx| {
            InstallAgentsModal::toggle(workspace, window, cx)
        })
        .log_err();
}

/// What `agent`'s "New … Terminal" button says.
pub(crate) fn new_terminal_label(name: &str) -> String {
    if is_chinese() {
        format!("新建 {name} 终端")
    } else {
        format!("New {name} Terminal")
    }
}

/// How long to wait for the new terminal's input to show before typing the
/// command into the shell instead.
const INPUT_TIMEOUT: Duration = Duration::from_secs(5);

/// Opens a terminal titled "Install <name>" with the agent's install command
/// in its input (with "Press Enter to run" below it), for the user to run
/// with Enter. When the terminal has no input (turned off, or a shell
/// without Solo's integration), the command is typed into the shell instead,
/// still without Enter. Once a command has run there, the inventory looks
/// again.
pub(crate) fn install_in_new_terminal(
    workspace: &Entity<Workspace>,
    spec: &'static AgentSpec,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(panel) = workspace.read(cx).panel::<TerminalPanel>(cx) else {
        return;
    };
    let directory = default_working_directory(workspace.read(cx), cx);
    let terminal = panel.update(cx, |panel, cx| {
        panel.add_terminal_shell(false, directory, RevealStrategy::Always, window, cx)
    });
    let workspace = workspace.downgrade();
    window
        .spawn(cx, async move |cx| {
            let terminal = terminal.await?;
            let view = cx.update(|_, cx| {
                let (Some(workspace), Some(terminal)) = (workspace.upgrade(), terminal.upgrade())
                else {
                    return None;
                };
                let view = terminal_views(&workspace, cx)
                    .into_iter()
                    .find(|view| view.read(cx).terminal() == &terminal)?;
                let title = if is_chinese() {
                    format!("安装 {}", spec.name)
                } else {
                    format!("Install {}", spec.name)
                };
                view.update(cx, |view, cx| {
                    view.set_custom_title(Some(title), cx);
                    view.queue_install_command(
                        spec.install_command.to_string(),
                        tr("Press Enter to run", "按 Enter 运行").into(),
                        cx,
                    );
                });
                look_again_after_commands(&terminal, view.downgrade(), cx);
                Some(view)
            })?;
            let Some(view) = view else {
                return anyhow::Ok(());
            };
            if cx.update(|_, cx| TerminalSettings::get_global(cx).input_editor)? {
                cx.background_executor().timer(INPUT_TIMEOUT).await;
            }
            cx.update(|_, cx| {
                view.update(cx, |view, cx| {
                    // Still waiting: the input never showed.
                    if view.take_pending_install().is_none() {
                        return;
                    }
                    view.terminal().update(cx, |terminal, _| {
                        terminal.write_init_command(spec.install_command.as_bytes().to_vec());
                    });
                })
            })?;
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
}

/// Looks for the agents again when a command that was started in `terminal`
/// finishes, once: the subscription ends then.
fn look_again_after_commands(
    terminal: &Entity<terminal::Terminal>,
    view: WeakEntity<crate::TerminalView>,
    cx: &mut App,
) {
    let started = Rc::new(Cell::new(false));
    let subscription: Rc<RefCell<Option<gpui::Subscription>>> = Rc::default();
    *subscription.borrow_mut() = Some(cx.subscribe(terminal, {
        let subscription = subscription.clone();
        move |_, event: &terminal::Event, cx| match event {
            terminal::Event::CommandStarted => {
                started.set(true);
                view.update(cx, |view, cx| view.clear_install_hint(cx)).ok();
            }
            terminal::Event::CommandFinished if started.replace(false) => {
                AgentInventory::global(cx).update(cx, |inventory, cx| inventory.look(false, cx));
                subscription.borrow_mut().take();
            }
            _ => {}
        }
    }));
}

/// What a row of the window says about an agent.
#[derive(Debug, PartialEq, Eq)]
enum RowState {
    Installed(Option<String>),
    /// Not installed, and installing it takes npm, which isn't there.
    NeedsNode,
    NotInstalled,
}

fn row_state(spec: &AgentSpec, inventory: &AgentInventory) -> RowState {
    match inventory.is_installed(spec) {
        Some(installed) => RowState::Installed(installed.version.clone()),
        None if spec.needs_npm && !inventory.npm_found() => RowState::NeedsNode,
        None => RowState::NotInstalled,
    }
}

pub(crate) struct InstallAgentsModal {
    workspace: WeakEntity<Workspace>,
    inventory: Entity<AgentInventory>,
    show_more: bool,
    focus_handle: FocusHandle,
    _observe_inventory: gpui::Subscription,
}

impl InstallAgentsModal {
    fn toggle(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
        let weak_workspace = workspace.weak_handle();
        if !workspace.project().read(cx).is_local() {
            return;
        }
        workspace.toggle_modal(window, cx, move |_, cx| {
            let inventory = AgentInventory::global(cx);
            inventory.update(cx, |inventory, cx| inventory.look(true, cx));
            Self {
                workspace: weak_workspace,
                _observe_inventory: cx.observe(&inventory, |_, _, cx| cx.notify()),
                inventory,
                show_more: false,
                focus_handle: cx.focus_handle(),
            }
        });
    }

    fn cancel(&mut self, _: &menu::Cancel, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }

    fn render_row(&self, index: usize, spec: &'static AgentSpec, cx: &mut Context<Self>) -> Div {
        let inventory = self.inventory.read(cx);
        let state = row_state(spec, inventory);
        let needs_node = state == RowState::NeedsNode;
        let installed = matches!(state, RowState::Installed(_));
        let status: SharedString = match &state {
            RowState::Installed(Some(version)) => {
                format!("{} · {version}", tr("Installed", "已安装")).into()
            }
            RowState::Installed(None) => tr("Installed", "已安装").into(),
            RowState::NeedsNode => tr("Install Node.js first", "要先装 Node.js").into(),
            RowState::NotInstalled => tr("Not installed", "没装").into(),
        };
        let command = spec.command;
        let action: AnyElement = if installed {
            Button::new(
                ("install-agents-new-terminal", index),
                tr("New Terminal", "新建终端"),
            )
            .style(ButtonStyle::Tinted(TintColor::Accent))
            .label_size(LabelSize::Small)
            .on_click(cx.listener(move |this, _, window, cx| {
                let workspace = this.workspace.clone();
                cx.emit(DismissEvent);
                workspace
                    .update(cx, |workspace, cx| {
                        crate::agent_context::start_in_new_terminal(
                            workspace,
                            spec.command,
                            window,
                            cx,
                        )
                    })
                    .log_err();
            }))
            .into_any_element()
        } else if needs_node {
            Button::new(
                ("install-agents-node", index),
                tr("Node.js Website", "Node.js 官网"),
            )
            .style(ButtonStyle::Outlined)
            .label_size(LabelSize::Small)
            .on_click(|_, _, cx| cx.open_url(NODE_DOWNLOAD_URL))
            .into_any_element()
        } else {
            Button::new(("install-agents-install", index), tr("Install", "安装"))
                .style(ButtonStyle::Filled)
                .label_size(LabelSize::Small)
                .on_click(cx.listener(move |this, _, window, cx| {
                    let workspace = this.workspace.clone();
                    cx.emit(DismissEvent);
                    if let Some(workspace) = workspace.upgrade() {
                        install_in_new_terminal(&workspace, spec, window, cx);
                    }
                }))
                .into_any_element()
        };
        let colors = cx.theme().colors();
        h_flex()
            .w_full()
            .gap_3()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(colors.border_variant)
            .child(render_agent_mark(
                spec.name,
                colors.elevated_surface_background,
                cx,
            ))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(Label::new(spec.name).untranslated())
                    .child(
                        Label::new(status)
                            .size(LabelSize::Small)
                            .color(if installed {
                                Color::Success
                            } else {
                                Color::Muted
                            })
                            .untranslated(),
                    ),
            )
            .child(
                div()
                    .debug_selector(move || format!("install-agents-action-{command}"))
                    .child(action),
            )
    }
}

impl EventEmitter<DismissEvent> for InstallAgentsModal {}

impl Focusable for InstallAgentsModal {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ModalView for InstallAgentsModal {}

impl Render for InstallAgentsModal {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let more = CATALOG.len() - COMMON_COUNT;
        let visible = if self.show_more {
            CATALOG.len()
        } else {
            COMMON_COUNT
        };
        let checking = self.inventory.read(cx).checking();
        let more_label = if self.show_more {
            tr("Show Fewer", "收起").to_string()
        } else if is_chinese() {
            format!("更多（{more}）")
        } else {
            format!("More ({more})")
        };
        let border = cx.theme().colors().border;
        let rows: Vec<Div> = CATALOG
            .iter()
            .enumerate()
            .take(visible)
            .map(|(index, spec)| self.render_row(index, spec, cx))
            .collect();

        v_flex()
            .w(rems(30.))
            .elevation_3(cx)
            .key_context("InstallAgents")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::cancel))
            .child(div().p_4().child(
                Label::new(tr("Install Agents", "安装 agent")).weight(gpui::FontWeight::BOLD),
            ))
            .child(v_flex().border_t_1().border_color(border).children(rows))
            .child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .p_2()
                    .child(
                        Button::new("install-agents-more", more_label)
                            .start_icon(Icon::new(if self.show_more {
                                IconName::ChevronUp
                            } else {
                                IconName::ChevronDown
                            }))
                            .label_size(LabelSize::Small)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.show_more = !this.show_more;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new(
                            "install-agents-check-again",
                            if checking {
                                tr("Checking…", "正在检查…")
                            } else {
                                tr("Check Again", "再检查一次")
                            },
                        )
                        .start_icon(Icon::new(IconName::RotateCw))
                        .label_size(LabelSize::Small)
                        .disabled(checking)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.inventory
                                .update(cx, |inventory, cx| inventory.look(true, cx));
                        })),
                    ),
            )
    }
}

/// A button of the empty state: the agent's mark and `label`.
pub(crate) fn agent_button(
    id: SharedString,
    agent: &InstalledAgent,
    label: SharedString,
    background: gpui::Hsla,
    cx: &App,
) -> ButtonLike {
    ButtonLike::new(id)
        .style(ButtonStyle::Outlined)
        .size(ButtonSize::Medium)
        .full_width()
        .child(
            h_flex()
                .gap_2()
                .child(render_agent_mark(agent.spec.name, background, cx))
                .child(Label::new(label).size(LabelSize::Small).untranslated()),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn names(scan: &Scan) -> Vec<&'static str> {
        scan.installed.iter().map(|agent| agent.spec.name).collect()
    }

    #[test]
    fn catalog_lists_the_common_agents_first_and_each_command_once() {
        let names: Vec<_> = CATALOG.iter().map(|spec| spec.name).collect();
        assert_eq!(
            &names[..COMMON_COUNT],
            [
                "Claude Code",
                "Codex",
                "Gemini CLI",
                "Copilot CLI",
                "OpenCode",
                "Qwen Code"
            ]
        );
        assert_eq!(CATALOG.len(), 11);
        let mut commands: Vec<_> = CATALOG.iter().map(|spec| spec.command).collect();
        commands.sort_unstable();
        commands.dedup();
        assert_eq!(commands.len(), CATALOG.len());
        for spec in &CATALOG {
            assert!(!spec.install_command.is_empty(), "{}", spec.name);
            assert!(spec.docs_url.starts_with("https://"), "{}", spec.name);
            assert_eq!(
                spec.needs_npm,
                spec.install_command.starts_with("npm "),
                "{}",
                spec.name
            );
        }
    }

    #[test]
    fn scan_lists_what_is_found_in_catalog_order() {
        let scan = scan_with(
            |command| {
                matches!(command, "gemini" | "claude" | "npm")
                    .then(|| PathBuf::from(format!("/bin/{command}")))
            },
            |_| Some("1.0.0".into()),
        );
        assert_eq!(names(&scan), ["Claude Code", "Gemini CLI"]);
        assert!(scan.npm_found);
        assert_eq!(scan.installed[0].version.as_deref(), Some("1.0.0"));

        let scan = scan_with(|_| None, |_| None);
        assert_eq!(scan, Scan::default());
    }

    // Looks asked for while one runs run once more, not once each; versions
    // already read stay when a look doesn't read them.
    #[gpui::test]
    async fn looks_asked_for_while_one_runs_run_once_more(cx: &mut gpui::TestAppContext) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        fn scanner(with_versions: bool) -> Scan {
            CALLS.fetch_add(1, Ordering::SeqCst);
            Scan {
                installed: vec![InstalledAgent {
                    spec: &CATALOG[0],
                    version: with_versions.then(|| "2.4.6".to_string()),
                }],
                npm_found: true,
            }
        }
        let inventory = cx.update(AgentInventory::global);
        inventory.update(cx, |inventory, _| inventory.set_scanner(scanner));
        inventory.update(cx, |inventory, cx| {
            inventory.look(false, cx);
            inventory.look(false, cx);
            inventory.look(false, cx);
        });
        cx.run_until_parked();
        assert_eq!(CALLS.load(Ordering::SeqCst), 2);
        inventory.read_with(cx, |inventory, _| {
            assert_eq!(inventory.installed()[0].version, None);
        });

        inventory.update(cx, |inventory, cx| inventory.look(true, cx));
        cx.run_until_parked();
        inventory.update(cx, |inventory, cx| inventory.look(false, cx));
        cx.run_until_parked();
        assert_eq!(CALLS.load(Ordering::SeqCst), 4);
        inventory.read_with(cx, |inventory, _| {
            assert_eq!(inventory.installed()[0].version.as_deref(), Some("2.4.6"));
        });
    }

    #[test]
    fn a_row_says_installed_needs_node_or_not_installed() {
        let mut inventory = AgentInventory {
            scan: Scan {
                installed: vec![InstalledAgent {
                    spec: &CATALOG[0],
                    version: Some("2.4.6".into()),
                }],
                npm_found: false,
            },
            scanned: true,
            checking: false,
            look_again: None,
            scanner: |_| Scan::default(),
            looking: None,
        };
        assert_eq!(
            row_state(&CATALOG[0], &inventory),
            RowState::Installed(Some("2.4.6".into()))
        );
        // Gemini CLI, OpenCode: installed with npm; Qwen Code: with a script.
        assert_eq!(row_state(&CATALOG[2], &inventory), RowState::NeedsNode);
        assert_eq!(row_state(&CATALOG[4], &inventory), RowState::NeedsNode);
        assert_eq!(row_state(&CATALOG[5], &inventory), RowState::NotInstalled);
        inventory.scan.npm_found = true;
        assert_eq!(row_state(&CATALOG[2], &inventory), RowState::NotInstalled);
    }

    #[test]
    fn empty_state_shows_at_most_four_in_catalog_order() {
        let everything = scan_with(|command| Some(PathBuf::from(command)), |_| None);
        let shown: Vec<_> = everything
            .installed
            .iter()
            .take(EMPTY_STATE_LIMIT)
            .map(|agent| agent.spec.name)
            .collect();
        assert_eq!(shown, ["Claude Code", "Codex", "Gemini CLI", "Copilot CLI"]);
    }

    #[test]
    fn versions_are_read_from_what_the_agents_print() {
        assert_eq!(
            parse_version("2.4.6 (Claude Code)\n").as_deref(),
            Some("2.4.6")
        );
        assert_eq!(
            parse_version("codex-cli 0.171.0").as_deref(),
            Some("0.171.0")
        );
        assert_eq!(
            parse_version("v1.2.3-beta.1").as_deref(),
            Some("1.2.3-beta.1")
        );
        assert_eq!(
            parse_version("Copilot CLI version 1.0.3.").as_deref(),
            Some("1.0.3")
        );
        assert_eq!(parse_version(""), None);
        assert_eq!(parse_version("error: unknown option"), None);
    }

    #[cfg(windows)]
    fn write_program(directory: &Path, name: &str, body: &str) -> PathBuf {
        let path = directory.join(format!("{name}.cmd"));
        fs::write(&path, format!("@echo off\r\n{body}\r\n")).unwrap();
        path
    }

    #[cfg(windows)]
    #[test]
    fn a_version_that_takes_too_long_stays_unknown() {
        let directory = tempfile::tempdir().unwrap();
        let fast = write_program(directory.path(), "fast", "echo fast 3.2.1");
        let slow = write_program(
            directory.path(),
            "slow",
            "ping -n 20 127.0.0.1 >nul\r\necho 9.9.9",
        );
        let start = std::time::Instant::now();
        assert_eq!(
            read_version(&fast, Duration::from_secs(5)).as_deref(),
            Some("3.2.1")
        );
        let slow_start = std::time::Instant::now();
        assert_eq!(read_version(&slow, Duration::from_millis(300)), None);
        assert!(
            slow_start.elapsed() < Duration::from_secs(3),
            "the wait ended at the timeout: {:?}",
            slow_start.elapsed()
        );
        assert!(start.elapsed() < Duration::from_secs(10));
        assert_eq!(
            read_version(&directory.path().join("missing.cmd"), VERSION_TIMEOUT),
            None
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_program_installed_after_the_process_started_is_found_from_the_registry_path() {
        use util::registry_environment::{RegistryEnvironment, RegistryValue, fresh_variables};

        let installed = tempfile::tempdir().unwrap();
        write_program(installed.path(), "gemini", "echo 0.9.1");
        let original = tempfile::tempdir().unwrap();
        write_program(original.path(), "claude", "echo 2.4.6");

        // Solo started before the install: its own PATH has no gemini.
        let process = vec![("PATH".to_string(), original.path().display().to_string())];
        let stale = path_among(&process);
        assert_eq!(locate("gemini", stale.as_deref()), None);
        assert!(locate("claude", stale.as_deref()).is_some());

        let registry = RegistryEnvironment {
            machine: Vec::new(),
            user: vec![RegistryValue {
                name: "Path".into(),
                value: installed.path().display().to_string(),
                expand: false,
            }],
        };
        let fresh = path_among(&fresh_variables(&registry, &process));
        let scan = scan_with(
            |command| locate(command, fresh.as_deref()),
            |program| read_version(program, VERSION_TIMEOUT),
        );
        assert_eq!(names(&scan), ["Claude Code", "Gemini CLI"]);
        assert_eq!(scan.installed[1].version.as_deref(), Some("0.9.1"));
    }
}
