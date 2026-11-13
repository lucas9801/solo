//! SOLO: what the language servers cost and how much they may do: the memory
//! and processes of each server with its children (`cargo` and `rustc` under
//! rust-analyzer), the per-project analysis modes (persisted here), and a
//! one-time question when a heavy server starts in a very large project.

use std::path::PathBuf;

use collections::{HashMap, HashSet};
use db::kvp::KeyValueStore;
use gpui::{App, AppContext as _, Entity, Global, WeakEntity};
use lsp::LanguageServerName;
use project::{
    LanguageServerProgress, Worktree,
    lsp_store::solo_server_modes::{self, AnalysisMode, LanguageServerModes},
};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
use util::{ResultExt as _, i18n::tr};
use workspace::{
    Workspace,
    notifications::{NotificationId, simple_message_notification::MessageNotification},
};

const MODES_KEY: &str = "solo_language_server_modes";
const PROMPTED_KEY: &str = "solo_large_project_prompted";
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

/// Sums up the process trees under `roots`. A process is identified by its
/// id, its parent's id and its name.
pub(crate) fn sum_process_trees(
    processes: &[(u32, Option<u32>, String, u64)],
    roots: &[u32],
) -> HashMap<u32, ProcessTreeUsage> {
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
    roots
        .iter()
        .filter_map(|root| {
            let root_index = *by_pid.get(root)?;
            let mut usage = ProcessTreeUsage::default();
            let mut seen = HashSet::default();
            let mut queue = vec![root_index];
            while let Some(index) = queue.pop() {
                if !seen.insert(index) {
                    continue;
                }
                let Some((pid, _, name, memory)) = processes.get(index) else {
                    continue;
                };
                usage.memory += memory;
                usage.processes += 1;
                if index != root_index && is_compiler(name) {
                    usage.compilers += 1;
                }
                if let Some(child_indices) = children.get(pid) {
                    queue.extend(child_indices.iter().copied());
                }
            }
            Some((*root, usage))
        })
        .collect()
}

/// Reads all processes and sums up the trees under `roots`. Slow enough
/// (tens of milliseconds) to belong on a background thread.
pub(crate) fn sample_process_trees(
    system: &mut System,
    roots: &[u32],
) -> HashMap<u32, ProcessTreeUsage> {
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_memory(),
    );
    let processes: Vec<(u32, Option<u32>, String, u64)> = system
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
    sum_process_trees(&processes, roots)
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
        format!(
            "这个项目有 {file_count} 个文件，{server} 在大项目里可能占用好几 GB 内存和大量 CPU。要它分析多少？（以后可以在状态栏的语言服务菜单里改）"
        )
    } else {
        format!(
            "This project has {file_count} files; in a project this large {server} can take several GB of memory and a lot of CPU. How much should it analyze? (You can change this later in the language servers menu in the status bar.)"
        )
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

#[cfg(test)]
mod tests {
    use super::*;

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
