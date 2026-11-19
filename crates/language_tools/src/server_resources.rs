//! SOLO: what the language servers cost and how much they may do: the memory
//! and processes of each server with its children (`cargo` and `rustc` under
//! rust-analyzer), the per-project analysis modes (persisted here), a
//! one-time question when a heavy server starts in a very large project, and
//! an alert when a project's servers take too much memory.

use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use collections::{HashMap, HashSet};
use db::kvp::KeyValueStore;
use gpui::{App, AppContext as _, Entity, Global, WeakEntity};
use lsp::LanguageServerName;
use project::{
    LanguageServerProgress, Worktree,
    lsp_store::solo_server_modes::{self, AnalysisMode, LanguageServerModes},
    project_settings::ProjectSettings,
};
use settings::Settings as _;
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
use ui::{Color, IconName};
use util::{ResultExt as _, i18n::tr};
use workspace::{
    Workspace,
    notifications::{NotificationId, simple_message_notification::MessageNotification},
};

const MODES_KEY: &str = "solo_language_server_modes";
const PROMPTED_KEY: &str = "solo_large_project_prompted";
/// How long the servers' memory must stay above the threshold before the
/// alert.
pub(crate) const MEMORY_ALERT_AFTER: Duration = Duration::from_secs(30);
/// Projects with more files than this get asked how much a heavy language
/// server should analyze.
pub(crate) const LARGE_PROJECT_FILE_COUNT: usize = 100_000;

/// Compilers that language servers start to check code; counted separately
/// because they take most of the memory while a check runs.
const COMPILER_NAMES: &[&str] = &[
    "rustc",
    "clippy-driver",
    "cl",
    "clang",
    "clang++",
    "gcc",
    "g++",
    "cc1plus",
    "javac",
    "csc",
];

/// Projects already asked about their size.
#[derive(Default)]
struct PromptedProjects(HashSet<PathBuf>);

impl Global for PromptedProjects {}

pub fn init(cx: &mut App) {
    let store = KeyValueStore::global(cx);
    let modes = store
        .read_kvp(MODES_KEY)
        .log_err()
        .flatten()
        .and_then(|json| serde_json::from_str::<LanguageServerModes>(&json).log_err())
        .unwrap_or_default();
    cx.set_global(modes);
    let prompted = store
        .read_kvp(PROMPTED_KEY)
        .log_err()
        .flatten()
        .and_then(|json| serde_json::from_str::<Vec<PathBuf>>(&json).log_err())
        .unwrap_or_default();
    cx.set_global(PromptedProjects(prompted.into_iter().collect()));
}

/// Sets how much `server` analyzes in the project at `project_root`, and
/// remembers it.
pub(crate) fn choose_mode(
    project_root: PathBuf,
    server: LanguageServerName,
    mode: AnalysisMode,
    cx: &mut App,
) {
    solo_server_modes::set_mode(project_root, server, mode, cx);
    save_modes(cx);
}

/// Sets how much all the language servers of a project analyze: those
/// given, those chosen there before and those that start there later; and
/// remembers it.
pub(crate) fn choose_project_mode(
    project_roots: &[PathBuf],
    servers: &[(PathBuf, LanguageServerName)],
    mode: AnalysisMode,
    cx: &mut App,
) {
    solo_server_modes::set_project_mode(project_roots, servers, mode, cx);
    save_modes(cx);
}

fn save_modes(cx: &mut App) {
    let Some(json) = serde_json::to_string(cx.global::<LanguageServerModes>()).log_err() else {
        return;
    };
    let store = KeyValueStore::global(cx);
    db::write_and_log(cx, move || async move {
        store.write_kvp(MODES_KEY.to_string(), json).await
    });
}

pub(crate) fn mode_label(mode: AnalysisMode) -> &'static str {
    match mode {
        AnalysisMode::Full => tr("Full analysis", "完整分析"),
        AnalysisMode::OpenFilesOnly => tr(
            "Pause background analysis (open files only)",
            "暂停后台分析（只分析打开的文件）",
        ),
        AnalysisMode::Off => tr("Off in this project", "在本项目关闭"),
    }
}

/// The short name of a mode, in the language servers menu.
pub(crate) fn short_mode_label(mode: AnalysisMode) -> &'static str {
    match mode {
        AnalysisMode::Full => tr("Full", "完整"),
        AnalysisMode::OpenFilesOnly => tr("Open files only", "仅打开的文件"),
        AnalysisMode::Off => tr("Off", "关闭"),
    }
}

/// The mode a server works in: servers that can't analyze only the open
/// files analyze everything.
pub(crate) fn effective_mode(server: &LanguageServerName, mode: AnalysisMode) -> AnalysisMode {
    if mode == AnalysisMode::OpenFilesOnly && !solo_server_modes::supports_open_files_only(server) {
        AnalysisMode::Full
    } else {
        mode
    }
}

/// Whether the user chose a mode for any server of these projects.
pub(crate) fn has_choices_in(project_roots: &[PathBuf], cx: &App) -> bool {
    cx.try_global::<LanguageServerModes>().is_some_and(|modes| {
        project_roots.iter().any(|root| {
            modes.project_modes.contains_key(root)
                || modes
                    .projects
                    .get(root)
                    .is_some_and(|servers| !servers.is_empty())
        })
    })
}

/// The modes offered for a server.
pub(crate) fn modes_for(server: &LanguageServerName) -> Vec<AnalysisMode> {
    if solo_server_modes::supports_open_files_only(server) {
        vec![
            AnalysisMode::Full,
            AnalysisMode::OpenFilesOnly,
            AnalysisMode::Off,
        ]
    } else {
        vec![AnalysisMode::Full, AnalysisMode::Off]
    }
}

/// The servers turned off in a project, to offer turning them on again.
pub(crate) fn servers_off_in(project_root: &std::path::Path, cx: &App) -> Vec<LanguageServerName> {
    let Some(modes) = cx.try_global::<LanguageServerModes>() else {
        return Vec::new();
    };
    let mut servers: Vec<LanguageServerName> = modes
        .projects
        .get(project_root)
        .into_iter()
        .flatten()
        .filter(|(_, mode)| **mode == AnalysisMode::Off)
        .map(|(server, _)| server.clone())
        .collect();
    servers.sort();
    servers
}

/// Memory and processes of a language server and everything it started.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ProcessTreeUsage {
    pub memory: u64,
    pub processes: usize,
    pub compilers: usize,
}

fn is_compiler(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    let name = name.strip_suffix(".exe").unwrap_or(&name);
    COMPILER_NAMES.contains(&name)
}

/// A process in a server's tree, found by the last full scan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TreeMember {
    /// The server at the root of the tree.
    pub root: u32,
    pub pid: u32,
    /// A compiler the server started (the server itself never counts).
    pub compiler: bool,
}

/// Finds the processes of the trees under `roots`. A process is identified
/// by its id, its parent's id and its name; its memory is not used here.
pub(crate) fn process_tree_members(
    processes: &[(u32, Option<u32>, String, u64)],
    roots: &[u32],
) -> Vec<TreeMember> {
    let mut children: HashMap<u32, Vec<usize>> = HashMap::default();
    for (index, (pid, parent, _, _)) in processes.iter().enumerate() {
        if let Some(parent) = parent
            && parent != pid
        {
            children.entry(*parent).or_default().push(index);
        }
    }
    let by_pid: HashMap<u32, usize> = processes
        .iter()
        .enumerate()
        .map(|(index, (pid, _, _, _))| (*pid, index))
        .collect();
    let mut members = Vec::new();
    for root in roots {
        let Some(&root_index) = by_pid.get(root) else {
            continue;
        };
        let mut seen = HashSet::default();
        let mut queue = vec![root_index];
        while let Some(index) = queue.pop() {
            if !seen.insert(index) {
                continue;
            }
            let Some((pid, _, name, _)) = processes.get(index) else {
                continue;
            };
            members.push(TreeMember {
                root: *root,
                pid: *pid,
                compiler: index != root_index && is_compiler(name),
            });
            if let Some(child_indices) = children.get(pid) {
                queue.extend(child_indices.iter().copied());
            }
        }
    }
    members
}

/// Sums up the members of each tree with the memory of each process.
pub(crate) fn sum_tree_members(
    members: &[TreeMember],
    mut memory: impl FnMut(u32) -> u64,
) -> HashMap<u32, ProcessTreeUsage> {
    let mut usage: HashMap<u32, ProcessTreeUsage> = HashMap::default();
    for member in members {
        let tree = usage.entry(member.root).or_default();
        tree.memory += memory(member.pid);
        tree.processes += 1;
        if member.compiler {
            tree.compilers += 1;
        }
    }
    usage
}

/// Sums up the process trees under `roots`. A process is identified by its
/// id, its parent's id and its name.
#[cfg(test)]
pub(crate) fn sum_process_trees(
    processes: &[(u32, Option<u32>, String, u64)],
    roots: &[u32],
) -> HashMap<u32, ProcessTreeUsage> {
    let memory: HashMap<u32, u64> = processes
        .iter()
        .map(|(pid, _, _, memory)| (*pid, *memory))
        .collect();
    sum_tree_members(&process_tree_members(processes, roots), |pid| {
        memory.get(&pid).copied().unwrap_or(0)
    })
}

/// How often the full process list is read while no server reports
/// progress, to find the processes the servers started since.
pub(crate) const FULL_SCAN_INTERVAL: Duration = Duration::from_secs(30);

/// Whether the next sample has to read the full process list (tens of
/// milliseconds of CPU on Windows) to find the trees again, rather than only
/// the memory of the processes found last time (microseconds). That is while
/// a server works (a check starts and ends compilers), when the servers
/// change, and every `FULL_SCAN_INTERVAL` otherwise.
pub(crate) fn needs_full_scan(
    last_scan: Option<Instant>,
    scanned_roots: &[u32],
    roots: &[u32],
    busy: bool,
    now: Instant,
) -> bool {
    busy || scanned_roots != roots
        || last_scan.is_none_or(|last_scan| now.duration_since(last_scan) >= FULL_SCAN_INTERVAL)
}

/// SOLO: samples the memory of the servers' process trees. Reading every
/// process on the machine is what costs, so it is done only when the trees
/// may have changed (see `needs_full_scan`); in between, only the processes
/// of the known trees are read. A process of a tree that exited makes the
/// sample read the full list again.
pub(crate) struct ProcessTreeSampler {
    system: System,
    roots: Vec<u32>,
    members: Vec<TreeMember>,
    last_scan: Option<Instant>,
    watched: watched::WatchedProcesses,
    full_scans: usize,
}

impl ProcessTreeSampler {
    pub(crate) fn new() -> Self {
        Self {
            system: System::new(),
            roots: Vec::new(),
            members: Vec::new(),
            last_scan: None,
            watched: watched::WatchedProcesses::default(),
            full_scans: 0,
        }
    }

    /// The memory of each tree under `roots`. `busy` is whether a server
    /// reports progress.
    pub(crate) fn sample(
        &mut self,
        roots: &[u32],
        busy: bool,
        now: Instant,
    ) -> HashMap<u32, ProcessTreeUsage> {
        let mut roots = roots.to_vec();
        roots.sort_unstable();
        roots.dedup();
        if !needs_full_scan(self.last_scan, &self.roots, &roots, busy, now) {
            let mut pids: Vec<u32> = self.members.iter().map(|member| member.pid).collect();
            pids.sort_unstable();
            pids.dedup();
            if let Some(memory) = self.watched.memory(&mut self.system, &pids) {
                return sum_tree_members(&self.members, |pid| {
                    memory.get(&pid).copied().unwrap_or(0)
                });
            }
        }
        self.full_scan(roots, now)
    }

    /// How many times the full process list was read.
    #[cfg(test)]
    pub(crate) fn full_scans(&self) -> usize {
        self.full_scans
    }

    fn full_scan(&mut self, roots: Vec<u32>, now: Instant) -> HashMap<u32, ProcessTreeUsage> {
        self.full_scans += 1;
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing().with_memory(),
        );
        let processes: Vec<(u32, Option<u32>, String, u64)> = self
            .system
            .processes()
            .iter()
            .map(|(pid, process)| {
                (
                    pid.as_u32(),
                    process.parent().map(Pid::as_u32),
                    process.name().to_string_lossy().into_owned(),
                    process.memory(),
                )
            })
            .collect();
        let memory: HashMap<u32, u64> = processes
            .iter()
            .map(|(pid, _, _, memory)| (*pid, *memory))
            .collect();
        self.members = process_tree_members(&processes, &roots);
        self.watched
            .watch(self.members.iter().map(|member| member.pid));
        self.roots = roots;
        self.last_scan = Some(now);
        sum_tree_members(&self.members, |pid| memory.get(&pid).copied().unwrap_or(0))
    }
}

/// Reads the memory of known processes without listing all of them.
#[cfg(windows)]
mod watched {
    use collections::{HashMap, HashSet};
    use sysinfo::System;
    use windows::Win32::{
        Foundation::{CloseHandle, HANDLE, STILL_ACTIVE},
        System::{
            ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS},
            Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
        },
    };

    /// An open handle also keeps the process id from being given to another
    /// process after this one exits.
    struct ProcessHandle(HANDLE);

    // SAFETY: a process handle can be used and closed from any thread.
    unsafe impl Send for ProcessHandle {}

    impl Drop for ProcessHandle {
        fn drop(&mut self) {
            // SAFETY: the handle was opened by `OpenProcess` and is closed once.
            unsafe {
                CloseHandle(self.0).ok();
            }
        }
    }

    #[derive(Default)]
    pub(super) struct WatchedProcesses {
        /// `None` for a process that cannot be opened; its memory counts as
        /// zero, as when the full list is read.
        handles: HashMap<u32, Option<ProcessHandle>>,
    }

    impl WatchedProcesses {
        pub(super) fn watch(&mut self, pids: impl IntoIterator<Item = u32>) {
            let pids: HashSet<u32> = pids.into_iter().collect();
            self.handles.retain(|pid, _| pids.contains(pid));
            for pid in pids {
                self.handles.entry(pid).or_insert_with(|| {
                    // SAFETY: plain call; the handle is owned by `ProcessHandle`.
                    unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
                        .ok()
                        .map(ProcessHandle)
                });
            }
        }

        /// The working set of each process, as `sysinfo` reports memory, or
        /// `None` when one of them exited or is not watched.
        pub(super) fn memory(
            &mut self,
            _system: &mut System,
            pids: &[u32],
        ) -> Option<HashMap<u32, u64>> {
            let mut memory = HashMap::default();
            for pid in pids {
                let Some(handle) = self.handles.get(pid)? else {
                    memory.insert(*pid, 0);
                    continue;
                };
                let mut exit_code = 0u32;
                // SAFETY: the handle is open and the out pointer is valid.
                unsafe { GetExitCodeProcess(handle.0, &mut exit_code) }.ok()?;
                if exit_code != STILL_ACTIVE.0 as u32 {
                    return None;
                }
                let mut counters = PROCESS_MEMORY_COUNTERS::default();
                // SAFETY: the handle is open and `counters` is as large as
                // the size passed.
                let working_set = unsafe {
                    GetProcessMemoryInfo(
                        handle.0,
                        &mut counters,
                        size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
                    )
                }
                .map_or(0, |()| counters.WorkingSetSize as u64);
                memory.insert(*pid, working_set);
            }
            Some(memory)
        }
    }
}

/// Reads the memory of known processes without listing all of them.
#[cfg(not(windows))]
mod watched {
    use collections::HashMap;
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

    #[derive(Default)]
    pub(super) struct WatchedProcesses;

    impl WatchedProcesses {
        pub(super) fn watch(&mut self, _pids: impl IntoIterator<Item = u32>) {}

        /// The memory of each process, or `None` when one of them exited.
        pub(super) fn memory(
            &mut self,
            system: &mut System,
            pids: &[u32],
        ) -> Option<HashMap<u32, u64>> {
            let pids: Vec<Pid> = pids.iter().copied().map(Pid::from_u32).collect();
            system.refresh_processes_specifics(
                ProcessesToUpdate::Some(&pids),
                true,
                ProcessRefreshKind::nothing().with_memory(),
            );
            pids.iter()
                .map(|pid| Some((pid.as_u32(), system.process(*pid)?.memory())))
                .collect()
        }
    }
}

pub(crate) fn format_memory(bytes: u64) -> String {
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    if bytes as f64 >= GB {
        format!("{:.1} GB", bytes as f64 / GB)
    } else {
        format!("{:.0} MB", bytes as f64 / MB)
    }
}

/// What a server is doing, from its progress reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ServerActivity {
    Idle,
    /// Checking the code for diagnostics (rust-analyzer's `cargo check`).
    Checking,
    /// Another progress report, by its title.
    Working(String),
}

pub(crate) fn server_activity<'a>(
    pending_work: impl IntoIterator<Item = &'a LanguageServerProgress>,
) -> ServerActivity {
    let mut title = None;
    for progress in pending_work {
        if progress.is_disk_based_diagnostics_progress {
            return ServerActivity::Checking;
        }
        if title.is_none() {
            title = progress.title.clone();
        }
    }
    title.map_or(ServerActivity::Idle, ServerActivity::Working)
}

/// "rust-analyzer: checking · 12 compiler processes · 6.3 GB".
pub(crate) fn describe_server(
    name: &str,
    activity: &ServerActivity,
    usage: Option<ProcessTreeUsage>,
) -> String {
    let separator = if util::i18n::is_chinese() {
        "："
    } else {
        ": "
    };
    format!("{name}{separator}{}", describe_usage(activity, usage))
}

/// "checking · 12 compiler processes · 6.3 GB".
pub(crate) fn describe_usage(activity: &ServerActivity, usage: Option<ProcessTreeUsage>) -> String {
    let chinese = util::i18n::is_chinese();
    let mut parts = Vec::new();
    match activity {
        ServerActivity::Idle => parts.push(tr("idle", "空闲").to_string()),
        ServerActivity::Checking => parts.push(tr("checking", "检查中").to_string()),
        ServerActivity::Working(title) => parts.push(title.clone()),
    }
    if let Some(usage) = usage {
        if usage.compilers > 0 {
            parts.push(if chinese {
                format!("{} 个编译进程", usage.compilers)
            } else if usage.compilers == 1 {
                "1 compiler process".to_string()
            } else {
                format!("{} compiler processes", usage.compilers)
            });
        }
        parts.push(format_memory(usage.memory));
    }
    parts.join(" · ")
}

/// Asks once per project, when a heavy server starts in a large one, how
/// much it should analyze.
pub(crate) fn maybe_ask_about_large_project(
    workspace: WeakEntity<Workspace>,
    worktree: Entity<Worktree>,
    server: LanguageServerName,
    cx: &mut App,
) {
    if !solo_server_modes::is_heavy_server(&server) {
        return;
    }
    // Asked only when the setting allows it and servers analyze everything
    // by default.
    let settings = &ProjectSettings::get_global(cx).global_lsp_settings;
    if !settings.ask_before_large_projects
        || solo_server_modes::default_mode(cx) != AnalysisMode::Full
    {
        return;
    }
    let project_root = worktree.read(cx).abs_path().to_path_buf();
    let already_decided = cx
        .try_global::<PromptedProjects>()
        .is_some_and(|prompted| prompted.0.contains(&project_root))
        || cx
            .try_global::<LanguageServerModes>()
            .is_some_and(|modes| modes.is_chosen(&project_root, &server));
    if already_decided {
        return;
    }
    let Some(scan_complete) = worktree
        .read(cx)
        .as_local()
        .map(|local| local.scan_complete())
    else {
        return;
    };
    cx.spawn(async move |cx| {
        scan_complete.await;
        cx.update(|cx| {
            let file_count = worktree.read(cx).snapshot().file_count();
            if file_count <= LARGE_PROJECT_FILE_COUNT {
                return;
            }
            let prompted = cx.default_global::<PromptedProjects>();
            if !prompted.0.insert(project_root.clone()) {
                return;
            }
            let prompted_paths: Vec<PathBuf> = prompted.0.iter().cloned().collect();
            if let Some(json) = serde_json::to_string(&prompted_paths).log_err() {
                let store = KeyValueStore::global(cx);
                db::write_and_log(cx, move || async move {
                    store.write_kvp(PROMPTED_KEY.to_string(), json).await
                });
            }
            show_large_project_prompt(workspace, project_root, server, file_count, cx);
        });
    })
    .detach();
}

fn show_large_project_prompt(
    workspace: WeakEntity<Workspace>,
    project_root: PathBuf,
    server: LanguageServerName,
    file_count: usize,
    cx: &mut App,
) {
    let message = if util::i18n::is_chinese() {
        format!("这个项目有 {file_count} 个文件，要 {server} 分析多少？")
    } else {
        format!("This project has {file_count} files. How much should {server} analyze?")
    };
    let modes = modes_for(&server);
    let id = NotificationId::named(format!("solo-large-project-{server}").into());
    workspace
        .update(cx, |workspace, cx| {
            workspace.show_notification(id, cx, |cx| {
                let choice = |mode: AnalysisMode| {
                    let project_root = project_root.clone();
                    let server = server.clone();
                    move |_: &mut gpui::Window, cx: &mut gpui::Context<MessageNotification>| {
                        choose_mode(project_root.clone(), server.clone(), mode, cx);
                    }
                };
                let mut notification = MessageNotification::new(message, cx)
                    .with_title(tr("Large project", "大项目"))
                    .show_suppress_button(false);
                if modes.contains(&AnalysisMode::OpenFilesOnly) {
                    notification = notification
                        .primary_message(mode_label(AnalysisMode::OpenFilesOnly))
                        .primary_on_click(choice(AnalysisMode::OpenFilesOnly))
                        .secondary_message(mode_label(AnalysisMode::Full))
                        .secondary_on_click(choice(AnalysisMode::Full));
                } else {
                    notification = notification
                        .primary_message(mode_label(AnalysisMode::Full))
                        .primary_on_click(choice(AnalysisMode::Full));
                }
                notification = notification
                    .secondary_menu_entry(mode_label(AnalysisMode::Off), choice(AnalysisMode::Off));
                cx.new(|_| notification)
            });
        })
        .log_err();
}

/// Watches a project's language server memory for the alert.
#[derive(Default)]
pub(crate) struct MemoryAlertTracker {
    over_since: Option<Instant>,
}

/// The projects already alerted about in this run, by their folders.
#[derive(Default)]
pub(crate) struct AlertedProjects(HashSet<Vec<PathBuf>>);

impl Global for AlertedProjects {}

impl MemoryAlertTracker {
    /// Whether to alert now about `project`, whose servers take `memory`
    /// bytes: when the memory has stayed above the threshold (`None`: the
    /// alert is off) for [`MEMORY_ALERT_AFTER`], at most once per project.
    pub(crate) fn should_alert(
        &mut self,
        memory: u64,
        threshold_gb: Option<u32>,
        now: Instant,
        project: &[PathBuf],
        alerted: &mut AlertedProjects,
    ) -> bool {
        const GB: u64 = 1024 * 1024 * 1024;
        let Some(threshold_gb) = threshold_gb else {
            self.over_since = None;
            return false;
        };
        if memory <= u64::from(threshold_gb.max(1)) * GB {
            self.over_since = None;
            return false;
        }
        let over_since = *self.over_since.get_or_insert(now);
        if now.duration_since(over_since) < MEMORY_ALERT_AFTER
            || project.is_empty()
            || alerted.0.contains(project)
        {
            return false;
        }
        alerted.0.insert(project.to_vec());
        true
    }
}

/// The alert threshold in GB, or `None` when the alert is off.
pub(crate) fn memory_alert_threshold(cx: &App) -> Option<u32> {
    let settings = &ProjectSettings::get_global(cx).global_lsp_settings;
    settings
        .memory_alert
        .then_some(settings.memory_alert_threshold_gb)
}

/// "clangd 4.4 GB · json-language-server 200 MB", the biggest first.
pub(crate) fn describe_memory(servers: &[(LanguageServerName, u64)]) -> String {
    let mut servers = servers.to_vec();
    servers.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    servers
        .iter()
        .map(|(name, memory)| format!("{name} {}", format_memory(*memory)))
        .collect::<Vec<_>>()
        .join(" · ")
}

/// Alerts that the project's language servers take `total` bytes, listing
/// each server's memory. Changes nothing by itself.
pub(crate) fn show_memory_alert(
    workspace: WeakEntity<Workspace>,
    project_roots: Vec<PathBuf>,
    servers: Vec<(PathBuf, LanguageServerName, u64)>,
    total: u64,
    cx: &mut App,
) {
    let title = if util::i18n::is_chinese() {
        format!("语言服务占用 {}", format_memory(total))
    } else {
        format!("Language services use {}", format_memory(total))
    };
    let body = describe_memory(
        &servers
            .iter()
            .map(|(_, name, memory)| (name.clone(), *memory))
            .collect::<Vec<_>>(),
    );
    let project_servers: Vec<(PathBuf, LanguageServerName)> = servers
        .into_iter()
        .map(|(root, name, _)| (root, name))
        .collect();
    let id = NotificationId::named("solo-language-server-memory".into());
    let workspace_for_settings = workspace.clone();
    workspace
        .update(cx, |workspace, cx| {
            workspace.show_notification(id, cx, |cx| {
                let choice = |mode: AnalysisMode| {
                    let project_roots = project_roots.clone();
                    let project_servers = project_servers.clone();
                    move |_: &mut gpui::Window, cx: &mut gpui::Context<MessageNotification>| {
                        choose_project_mode(&project_roots, &project_servers, mode, cx);
                    }
                };
                let workspace = workspace_for_settings.clone();
                let notification = MessageNotification::new(body, cx)
                    .with_title(title)
                    .content_icon(IconName::Warning, Color::Warning)
                    .show_suppress_button(false)
                    .primary_message(tr("Analyze Open Files Only", "仅分析打开的文件"))
                    .primary_on_click(choice(AnalysisMode::OpenFilesOnly))
                    .secondary_message(tr(
                        "Turn Off Language Services in This Project",
                        "关闭本项目语言服务",
                    ))
                    .secondary_on_click(choice(AnalysisMode::Off))
                    .tertiary_message(tr("Don't Alert Again", "不再提醒"))
                    .tertiary_on_click(move |_, cx| {
                        let Ok(fs) = workspace
                            .read_with(cx, |workspace, _| workspace.app_state().fs.clone())
                        else {
                            return;
                        };
                        settings::update_settings_file(fs, cx, |settings, _| {
                            settings
                                .global_lsp_settings
                                .get_or_insert_default()
                                .memory_alert
                                .get_or_insert_default()
                                .enabled = Some(false);
                        });
                    });
                cx.new(|_| notification)
            });
        })
        .log_err();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_alert_after_30_seconds_once_per_project() {
        const GB: u64 = 1024 * 1024 * 1024;
        let start = Instant::now();
        let at = |seconds: u64| start + Duration::from_secs(seconds);
        let project = vec![PathBuf::from("/a")];
        let mut alerted = AlertedProjects::default();
        let mut tracker = MemoryAlertTracker::default();

        // At or below the threshold: nothing.
        assert!(!tracker.should_alert(4 * GB, Some(4), at(0), &project, &mut alerted));
        // Above it, not before 30 seconds.
        assert!(!tracker.should_alert(5 * GB, Some(4), at(4), &project, &mut alerted));
        assert!(!tracker.should_alert(5 * GB, Some(4), at(30), &project, &mut alerted));
        // Dropping below starts over.
        assert!(!tracker.should_alert(3 * GB, Some(4), at(32), &project, &mut alerted));
        assert!(!tracker.should_alert(5 * GB, Some(4), at(36), &project, &mut alerted));
        assert!(!tracker.should_alert(5 * GB, Some(4), at(60), &project, &mut alerted));
        assert!(tracker.should_alert(5 * GB, Some(4), at(66), &project, &mut alerted));
        // Once per project and run, also from another window.
        assert!(!tracker.should_alert(9 * GB, Some(4), at(200), &project, &mut alerted));
        let mut other_window = MemoryAlertTracker::default();
        assert!(!other_window.should_alert(9 * GB, Some(4), at(0), &project, &mut alerted));
        assert!(!other_window.should_alert(9 * GB, Some(4), at(40), &project, &mut alerted));

        // Another project is alerted about on its own.
        let other = vec![PathBuf::from("/b")];
        let mut tracker = MemoryAlertTracker::default();
        assert!(!tracker.should_alert(5 * GB, Some(4), at(0), &other, &mut alerted));
        assert!(tracker.should_alert(5 * GB, Some(4), at(30), &other, &mut alerted));

        // A higher threshold, and the alert turned off.
        let third = vec![PathBuf::from("/c")];
        let mut tracker = MemoryAlertTracker::default();
        assert!(!tracker.should_alert(5 * GB, Some(8), at(0), &third, &mut alerted));
        assert!(!tracker.should_alert(5 * GB, Some(8), at(40), &third, &mut alerted));
        assert!(!tracker.should_alert(9 * GB, None, at(0), &third, &mut alerted));
        assert!(!tracker.should_alert(9 * GB, None, at(40), &third, &mut alerted));
    }

    #[test]
    fn describes_memory_and_modes() {
        const MB: u64 = 1024 * 1024;
        let text = describe_memory(&[
            (
                LanguageServerName::new_static("json-language-server"),
                200 * MB,
            ),
            (LanguageServerName::new_static("clangd"), 4500 * MB),
        ]);
        assert_eq!(text, "clangd 4.4 GB · json-language-server 200 MB");

        let json = LanguageServerName::new_static("json-language-server");
        let clangd = LanguageServerName::new_static("clangd");
        assert_eq!(
            effective_mode(&json, AnalysisMode::OpenFilesOnly),
            AnalysisMode::Full
        );
        assert_eq!(
            effective_mode(&clangd, AnalysisMode::OpenFilesOnly),
            AnalysisMode::OpenFilesOnly
        );
        assert_eq!(effective_mode(&json, AnalysisMode::Off), AnalysisMode::Off);
    }

    #[test]
    fn sums_process_trees_with_children() {
        const MB: u64 = 1024 * 1024;
        let processes = vec![
            (1, None, "solo.exe".to_string(), 500 * MB),
            (10, Some(1), "rust-analyzer.exe".to_string(), 240 * MB),
            (
                11,
                Some(10),
                "rust-analyzer-proc-macro-srv.exe".to_string(),
                315 * MB,
            ),
            (12, Some(10), "cargo.exe".to_string(), 20 * MB),
            (13, Some(12), "rustc.exe".to_string(), 1500 * MB),
            (14, Some(12), "rustc.exe".to_string(), 900 * MB),
            (20, Some(1), "clangd.exe".to_string(), 100 * MB),
            // A process that is its own parent must not loop.
            (30, Some(30), "odd.exe".to_string(), MB),
        ];
        let usage = sum_process_trees(&processes, &[10, 20, 30, 99]);
        assert_eq!(
            usage.get(&10),
            Some(&ProcessTreeUsage {
                memory: (240 + 315 + 20 + 1500 + 900) * MB,
                processes: 5,
                compilers: 2,
            })
        );
        assert_eq!(usage.get(&20).map(|usage| usage.processes), Some(1));
        assert_eq!(usage.get(&30).map(|usage| usage.processes), Some(1));
        assert!(!usage.contains_key(&99));
    }

    #[test]
    fn reads_all_processes_only_when_trees_may_change() {
        let start = Instant::now();
        let at = |seconds: u64| start + Duration::from_secs(seconds);
        // The first sample, and new or stopped servers.
        assert!(needs_full_scan(None, &[], &[10], false, at(0)));
        assert!(needs_full_scan(Some(at(0)), &[10], &[10, 20], false, at(4)));
        assert!(needs_full_scan(Some(at(0)), &[10, 20], &[20], false, at(4)));
        // Idle servers: only every 30 seconds.
        assert!(!needs_full_scan(Some(at(0)), &[10], &[10], false, at(4)));
        assert!(!needs_full_scan(Some(at(0)), &[10], &[10], false, at(29)));
        assert!(needs_full_scan(Some(at(0)), &[10], &[10], false, at(30)));
        // A working server may start compilers at any time.
        assert!(needs_full_scan(Some(at(0)), &[10], &[10], true, at(4)));
    }

    #[test]
    #[allow(
        clippy::disallowed_methods,
        reason = "a plain child process for the test to find"
    )]
    fn samples_known_trees_without_reading_all_processes() {
        let mut child = if cfg!(windows) {
            std::process::Command::new("ping")
                .args(["-n", "60", "127.0.0.1"])
                .stdout(std::process::Stdio::null())
                .spawn()
        } else {
            std::process::Command::new("sleep").arg("60").spawn()
        }
        .expect("start a child process");
        let root = std::process::id();
        let start = Instant::now();
        let at = |seconds: u64| start + Duration::from_secs(seconds);
        let mut sampler = ProcessTreeSampler::new();

        // The first sample reads all processes and finds the child.
        let usage = sampler.sample(&[root], false, at(0));
        let tree = usage.get(&root).copied().expect("the test process");
        assert!(tree.processes >= 2, "{tree:?}");
        assert!(tree.memory > 0);
        assert_eq!(sampler.full_scans(), 1);

        // Idle: only the known processes are read, with the same tree.
        let usage = sampler.sample(&[root], false, at(4));
        let fast = usage.get(&root).copied().expect("the test process");
        assert_eq!(fast.processes, tree.processes);
        assert!(fast.memory > 0);
        assert_eq!(sampler.full_scans(), 1);

        // A busy server, or 30 seconds, read them all again.
        sampler.sample(&[root], true, at(8));
        assert_eq!(sampler.full_scans(), 2);
        sampler.sample(&[root], false, at(12));
        assert_eq!(sampler.full_scans(), 2);
        sampler.sample(&[root], false, at(38));
        assert_eq!(sampler.full_scans(), 3);

        // A process of the tree that exits makes the next sample read them
        // all again, without it.
        child.kill().ok();
        child.wait().ok();
        let usage = sampler.sample(&[root], false, at(42));
        assert_eq!(sampler.full_scans(), 4);
        let after = usage.get(&root).copied().expect("the test process");
        assert_eq!(after.processes, tree.processes - 1);

        // An unknown server has no usage.
        let usage = sampler.sample(&[u32::MAX - 1], false, at(46));
        assert!(usage.is_empty());
    }

    #[test]
    fn describes_a_server() {
        let usage = ProcessTreeUsage {
            memory: 6_764_573_491,
            processes: 15,
            compilers: 12,
        };
        let text = describe_server("rust-analyzer", &ServerActivity::Checking, Some(usage));
        if util::i18n::is_chinese() {
            assert_eq!(text, "rust-analyzer：检查中 · 12 个编译进程 · 6.3 GB");
        } else {
            assert_eq!(
                text,
                "rust-analyzer: checking · 12 compiler processes · 6.3 GB"
            );
        }
        assert_eq!(format_memory(240 * 1024 * 1024), "240 MB");
    }
}
