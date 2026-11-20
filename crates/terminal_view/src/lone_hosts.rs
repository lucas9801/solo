//! SOLO: terminal hosts no terminal of this Solo uses. A host outlives Solo
//! when its terminal was busy as Solo went (quit, restarted for an update,
//! crashed), for the next Solo to restore the terminal and connect to it
//! again. A saved terminal of a workspace tab not open yet (a tab opens when
//! it's first shown) waits for its tab, which restores it. When that doesn't
//! happen (its workspace isn't a tab, or its saved terminal is gone), the
//! host would run on unseen, with its agent or build. Hosts list themselves
//! in Solo's data folder (see `terminal::running_hosts`); after restoring the
//! session, Solo opens each one no terminal connected to as a terminal in
//! the tab of its project (by its directory, as a terminal's tab goes; that
//! tab opens in the background if it isn't open), else in the current tab of
//! its first window, and offers to end those it can't open.
//!
//! Hosts restarting Solo (for an update) or a crash left running keep
//! running until their tab opens. Quitting Solo asks about them too, naming
//! their tabs, and ends them (unless `terminal.keep_running_on_quit` is on).

use collections::HashSet;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use gpui::{App, AppContext as _, AsyncApp, Entity, Task, WindowHandle};
use std::path::PathBuf;
use std::time::Duration;
use terminal::RunningHost;
use util::ResultExt as _;
use util::i18n::tr;
use util::path_list::{PathList, SerializedPathList};
use workspace::notifications::NotificationId;
use workspace::notifications::simple_message_notification::MessageNotification;
use settings::Settings as _;
use terminal::terminal_settings::{TerminalConfirmClose, TerminalSettings};
use workspace::item::RunningProgram;
use workspace::solo_quit::{BackgroundProgram, BackgroundPrograms};
use workspace::{MultiWorkspace, ProjectGroupKey, Workspace, WorkspaceId};

use crate::TerminalView;
use crate::persistence::TerminalDb;
use crate::terminal_panel::TerminalPanel;
use crate::workspace_grouping::{self, Destination};

/// After the session is restored, its terminals still connect to their
/// hosts for a moment.
const FIRST_LOOK: Duration = Duration::from_secs(5);
/// Hosts a saved terminal refers to get longer: its workspace may still be
/// opening.
const SECOND_LOOK: Duration = Duration::from_secs(60);
/// How long a workspace opened later (a tab opened) gets to restore its
/// terminals before its hosts no terminal took are looked at.
const TAB_RESTORE: Duration = Duration::from_secs(15);

struct LoneTerminalHosts;

/// Lets quitting ask about, and end, the hosts no terminal uses.
pub(crate) fn init(cx: &mut App) {
    cx.set_global(BackgroundPrograms {
        list: background_programs,
        end: end_background_programs,
    });
}

/// Whether quitting Solo now leaves what runs in terminals running.
fn quitting_keeps_running(cx: &App) -> bool {
    workspace::solo_quit::is_restarting(cx)
        || (cx.has_global::<settings::SettingsStore>()
            && TerminalSettings::get_global(cx).keep_running_on_quit)
}

/// The programs hosts no terminal uses run, by their tab, which quitting
/// would end and the quit prompt is to name, by the same rules as those of
/// open tabs (`terminal.confirm_close`). Looked up in the background: the
/// hosts' list, the saved terminals and the processes.
fn background_programs(cx: &App) -> Task<Vec<BackgroundProgram>> {
    if quitting_keeps_running(cx) {
        return Task::ready(Vec::new());
    }
    let settings = TerminalSettings::get_global(cx);
    let confirm_close = settings.confirm_close;
    if confirm_close == TerminalConfirmClose::Never {
        return Task::ready(Vec::new());
    }
    let extra = settings.agent_commands.clone();
    let db = TerminalDb::global(cx);
    cx.background_spawn(async move {
        let lone: Vec<RunningHost> = terminal::running_hosts()
            .into_iter()
            .filter(|host| host.is_left_alone())
            .collect();
        if lone.is_empty() {
            return Vec::new();
        }
        let saved = saved_hosts(&db);
        let shells: Vec<u32> = lone.iter().filter_map(|host| host.child_pid).collect();
        let mut programs = terminal::shell_programs::programs_under(&shells);
        background_rows(lone, &saved, &mut programs, confirm_close, &extra)
    })
}

#[cfg(test)]
pub(crate) fn background_rows_for_test(
    lone: Vec<RunningHost>,
    programs: &mut collections::HashMap<u32, Vec<terminal::ShellProgram>>,
    confirm_close: TerminalConfirmClose,
) -> Vec<BackgroundProgram> {
    background_rows(lone, &[], programs, confirm_close, &[])
}

/// The quit prompt's rows for the hosts no terminal uses, given what runs
/// under each one's shell.
fn background_rows(
    lone: Vec<RunningHost>,
    saved: &[SavedHost],
    programs: &mut collections::HashMap<u32, Vec<terminal::ShellProgram>>,
    confirm_close: TerminalConfirmClose,
    extra: &[String],
) -> Vec<BackgroundProgram> {
    lone.into_iter()
        .filter_map(|host| {
            let tab = saved
                .iter()
                .find(|saved| saved.pipe == host.pipe)
                .and_then(|saved| saved.paths.ordered_paths().next().cloned())
                .or_else(|| host.working_directory.clone())
                .map(|path| workspace_grouping::folder_name(&path))
                .unwrap_or_default();
            let programs = host
                .child_pid
                .and_then(|pid| programs.remove(&pid))
                .unwrap_or_default();
            let agent = programs
                .iter()
                .find(|program| crate::command_blocks::is_agent_command(&program.command, extra));
            // As in open tabs: agents, and with "commands" any command.
            if agent.is_none() && confirm_close != TerminalConfirmClose::Commands {
                return None;
            }
            let name = agent
                .or(programs.first())
                .and_then(|program| program.command.split(' ').next())
                .map(str::to_string)
                .unwrap_or_else(|| tr("a program", "一个程序").to_string());
            Some(BackgroundProgram {
                tab: tab.into(),
                program: RunningProgram {
                    name: name.into(),
                    status: None,
                },
            })
        })
        .collect()
}

/// Ends the hosts no terminal uses: the user quit Solo.
fn end_background_programs(cx: &mut App) {
    if quitting_keeps_running(cx) {
        return;
    }
    // Now: Solo is about to go.
    for host in terminal::running_hosts() {
        if host.is_left_alone() {
            terminal::end_running_host(&host.pipe).log_err();
        }
    }
}

/// Looks for hosts no terminal uses, once the session is restored.
pub fn look_for_lone_terminal_hosts(cx: &mut App) {
    let db = TerminalDb::global(cx);
    let seen: Rc<RefCell<HashSet<String>>> = Rc::default();
    let looked: Arc<AtomicBool> = Arc::default();
    cx.spawn({
        let db = db.clone();
        let seen = seen.clone();
        let looked = looked.clone();
        async move |cx| {
            cx.background_executor().timer(FIRST_LOOK).await;
            look(&db, false, &seen, cx).await;
            cx.background_executor().timer(SECOND_LOOK).await;
            look(&db, true, &seen, cx).await;
            looked.store(true, Ordering::SeqCst);
        }
    })
    .detach();
    // A tab opened later restores its saved terminals; a host one of them
    // waited for that it didn't take (its record was stale, say) would
    // wait on unseen: look again once it has restored.
    cx.observe_new(move |_: &mut Workspace, _, cx| {
        let db = db.clone();
        let seen = seen.clone();
        let looked = looked.clone();
        cx.spawn(async move |_, cx| {
            cx.background_executor().timer(TAB_RESTORE).await;
            if looked.load(Ordering::SeqCst) {
                look(&db, true, &seen, cx).await;
            }
        })
        .detach();
    })
    .detach();
}

/// A saved terminal's host, and the workspace it was saved in.
struct SavedHost {
    pipe: String,
    workspace_id: WorkspaceId,
    paths: PathList,
    /// The folders of its project: for a linked git worktree, its main
    /// repository's.
    identity_paths: Option<PathList>,
}

fn saved_hosts(db: &TerminalDb) -> Vec<SavedHost> {
    db.terminal_hosts_with_workspaces()
        .log_err()
        .unwrap_or_default()
        .into_iter()
        .map(
            |(pipe, workspace_id, paths, order, identity_paths, identity_order)| SavedHost {
                pipe,
                workspace_id,
                paths: PathList::deserialize(&SerializedPathList { paths, order }),
                identity_paths: identity_paths.map(|paths| {
                    PathList::deserialize(&SerializedPathList {
                        paths,
                        order: identity_order.unwrap_or_default(),
                    })
                }),
            },
        )
        .collect()
}

/// A workspace tab of a window, open or not yet.
#[derive(Clone)]
struct Tab<W> {
    /// Its workspace, once the tab is open.
    workspace: Option<W>,
    database_id: Option<WorkspaceId>,
    paths: PathList,
}

/// Where the saved terminal of a host goes.
#[derive(Debug, PartialEq)]
enum SavedTab {
    /// In the open tab of this index, which restored it, or didn't (then
    /// the host is opened there).
    Open(usize),
    /// In a tab not open yet, which restores it when it opens.
    NotOpenYet,
    /// In the project of the tab of this index, not open yet, as a linked
    /// git worktree of it: the tab opens its main repository's workspace,
    /// which doesn't restore it, so the host is opened there.
    Worktree(usize),
    /// In none.
    None,
}

/// Which of the `tabs` the saved terminal of `saved` belongs in.
fn saved_tab<W>(saved: &SavedHost, tabs: &[Tab<W>]) -> SavedTab {
    if let Some(ix) = tabs.iter().position(|tab| {
        tab.workspace.is_some() && tab.database_id == Some(saved.workspace_id)
    }) {
        return SavedTab::Open(ix);
    }
    // A tab opens the workspace saved for its folders.
    if tabs
        .iter()
        .any(|tab| tab.workspace.is_none() && tab.paths == saved.paths)
    {
        return SavedTab::NotOpenYet;
    }
    if let Some(identity) = &saved.identity_paths
        && let Some(ix) = tabs
            .iter()
            .position(|tab| tab.workspace.is_none() && tab.paths == *identity)
    {
        return SavedTab::Worktree(ix);
    }
    SavedTab::None
}

/// A tab of a window with a local project.
struct WindowTab {
    window: WindowHandle<MultiWorkspace>,
    multi_workspace: Entity<MultiWorkspace>,
    tab: Tab<Entity<Workspace>>,
    key: Option<ProjectGroupKey>,
}

/// The tabs of all windows, open or not yet, with local projects.
fn window_tabs(cx: &App) -> Vec<WindowTab> {
    let mut result = Vec::new();
    for window in cx.windows() {
        let Some(window) = window.downcast::<MultiWorkspace>() else {
            continue;
        };
        let Ok(multi_workspace) = window.entity(cx) else {
            continue;
        };
        let mut keys = Vec::new();
        for workspace in multi_workspace.read(cx).workspaces() {
            let read = workspace.read(cx);
            if !read.project().read(cx).is_local() {
                continue;
            }
            let key = read.project_group_key(cx);
            keys.push(key.clone());
            result.push(WindowTab {
                window,
                multi_workspace: multi_workspace.clone(),
                tab: Tab {
                    workspace: Some(workspace.clone()),
                    database_id: read.database_id(),
                    paths: PathList::new(&read.root_paths(cx)),
                },
                key: Some(key),
            });
        }
        for group in multi_workspace.read(cx).project_groups(cx) {
            if group.key.host().is_some()
                || !group.workspaces.is_empty()
                || keys.contains(&group.key)
                || group.key.path_list().paths().is_empty()
            {
                continue;
            }
            result.push(WindowTab {
                window,
                multi_workspace: multi_workspace.clone(),
                tab: Tab {
                    workspace: None,
                    database_id: None,
                    paths: group.key.path_list().clone(),
                },
                key: Some(group.key),
            });
        }
    }
    result
}

async fn look(
    db: &TerminalDb,
    with_saved: bool,
    seen: &Rc<RefCell<HashSet<String>>>,
    cx: &mut AsyncApp,
) {
    let saved = saved_hosts(db);
    let hosts = cx
        .background_spawn(async move { terminal::running_hosts() })
        .await;
    let lone: Vec<RunningHost> = hosts
        .into_iter()
        .filter(|host| host.is_left_alone() && !seen.borrow().contains(&host.pipe))
        .collect();
    if lone.is_empty() {
        return;
    }
    let windows = cx.update(|cx| window_tabs(cx));
    let tabs: Vec<Tab<Entity<Workspace>>> =
        windows.iter().map(|window| window.tab.clone()).collect();
    let mut not_opened = Vec::new();
    for host in lone {
        let mut place = None;
        // The newest record of its pipe (saved first).
        if let Some(saved) = saved.iter().find(|saved| saved.pipe == host.pipe) {
            // Its workspace may still be restoring it.
            if !with_saved {
                continue;
            }
            match saved_tab(saved, &tabs) {
                SavedTab::Open(ix) | SavedTab::Worktree(ix) => place = Some(ix),
                SavedTab::NotOpenYet => {
                    log::info!(
                        "terminal host {} ({}) waits for its tab to open",
                        host.host_pid,
                        host.pipe
                    );
                    continue;
                }
                SavedTab::None => {}
            }
        }
        seen.borrow_mut().insert(host.pipe.clone());
        log::info!(
            "terminal host {} ({}) runs {} with no terminal, opening it",
            host.host_pid,
            host.pipe,
            host.command_line
        );
        let place = match place {
            Some(ix) => Some(ix),
            None => tab_for_host(&host, &tabs, cx).await,
        };
        let opened = match place {
            Some(ix) => open_in_tab(&host, &windows[ix], cx).await,
            None => open_as_terminal(&host, cx).await,
        };
        if let Err(error) = opened {
            log::warn!(
                "couldn't open the terminal host {} as a terminal: {error:#}",
                host.pipe
            );
            // It may have ended meanwhile.
            if terminal::running_hosts()
                .iter()
                .any(|running| running.pipe == host.pipe && running.is_left_alone())
            {
                not_opened.push(host);
            }
        }
    }
    if !not_opened.is_empty() {
        cx.update(|cx| offer_to_end(not_opened, cx));
    }
}

/// Of the `tabs`, the one of the project the host runs in, by its
/// directory, as a terminal's tab goes (see `workspace_grouping`).
async fn tab_for_host<W>(
    host: &RunningHost,
    tabs: &[Tab<W>],
    cx: &mut AsyncApp,
) -> Option<usize> {
    let directory = host.working_directory.clone()?;
    let fs = cx.update(|cx| <dyn project::Fs>::global(cx));
    let folders: Vec<Vec<PathBuf>> = tabs
        .iter()
        .map(|tab| tab.paths.ordered_paths().cloned().collect())
        .collect();
    workspace_grouping::tab_for_directory(fs.as_ref(), &directory, &folders).await
}

/// The environment of a terminal only to connect to `host`.
fn reconnect_env(host: &RunningHost) -> collections::HashMap<String, String> {
    [
        (terminal::HOST_RECONNECT_ENV.to_string(), host.pipe.clone()),
        (
            terminal::HOST_RECONNECT_ONLY_ENV.to_string(),
            "1".to_string(),
        ),
    ]
    .into_iter()
    .collect()
}

/// Opens `host` as a terminal in the terminal panel of `tab`, opening the
/// tab in the background if it isn't open.
async fn open_in_tab(host: &RunningHost, tab: &WindowTab, cx: &mut AsyncApp) -> anyhow::Result<()> {
    let workspace = match &tab.tab.workspace {
        Some(workspace) => workspace.clone(),
        None => {
            let destination = Destination {
                key: tab.key.clone(),
                paths: tab.tab.paths.ordered_paths().cloned().collect(),
                workspace: None,
            };
            let opening = tab.window.update(cx, |_, window, cx| {
                workspace_grouping::destination_workspace(
                    &tab.multi_workspace,
                    &destination,
                    false,
                    window,
                    cx,
                )
            })?;
            opening
                .await
                .ok_or_else(|| anyhow::anyhow!("couldn't open the tab"))?
        }
    };
    let panel = workspace_grouping::panel_ready_for_terminal(&workspace, cx)
        .await
        .ok_or_else(|| anyhow::anyhow!("no terminal panel"))?;
    let added = add_to_panel(host, tab.window, &workspace, &panel, cx).await;
    workspace_grouping::done_adding_terminal(&panel, cx);
    added
}

async fn add_to_panel(
    host: &RunningHost,
    window: WindowHandle<MultiWorkspace>,
    workspace: &Entity<Workspace>,
    panel: &Entity<TerminalPanel>,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    let project = workspace.read_with(cx, |workspace, _| workspace.project().clone());
    let env = reconnect_env(host);
    let cwd = host.working_directory.clone();
    let terminal = project
        .update(cx, |project, cx| {
            project.create_terminal_shell_with_env(cwd, env, cx)
        })
        .await?;
    let pipe = host.pipe.clone();
    window.update(cx, |_, window, cx| {
        let view = cx.new(|cx| {
            let mut view = TerminalView::new(
                terminal,
                workspace.downgrade(),
                workspace.read(cx).database_id(),
                project.downgrade(),
                window,
                cx,
            );
            view.saved_host = Some(pipe);
            // Kept running from now on, as a restored terminal is.
            view.update_keep_running(cx);
            view
        });
        let pane = panel.read(cx).pane_for_moved_terminal(cx);
        pane.update(cx, |pane, cx| {
            pane.add_item(Box::new(view), false, false, None, window, cx)
        });
    })?;
    Ok(())
}

/// The first window with a local project, where a terminal can connect to
/// a host on this computer.
fn local_workspace(cx: &App) -> Option<(WindowHandle<MultiWorkspace>, Entity<Workspace>)> {
    cx.windows().into_iter().find_map(|window| {
        let window = window.downcast::<MultiWorkspace>()?;
        let workspace = window.read(cx).ok()?.workspace().clone();
        workspace
            .read(cx)
            .project()
            .read(cx)
            .is_local()
            .then_some((window, workspace))
    })
}

/// Opens `host` as a terminal in the current tab of the first window, when
/// no tab has its project; its bar says it's in no folder of the tab.
async fn open_as_terminal(host: &RunningHost, cx: &mut AsyncApp) -> anyhow::Result<()> {
    let (window, workspace) = cx
        .update(|cx| local_workspace(cx))
        .ok_or_else(|| anyhow::anyhow!("no window with a local project"))?;
    let env = reconnect_env(host);
    let cwd = host.working_directory.clone();
    let opened = window.update(cx, |_, window, cx| {
        workspace.update(cx, |workspace, cx| {
            TerminalPanel::add_center_terminal(workspace, window, cx, move |project, cx| {
                project.create_terminal_shell_with_env(cwd, env, cx)
            })
        })
    })?;
    opened.await?;
    Ok(())
}

fn offer_to_end(hosts: Vec<RunningHost>, cx: &mut App) {
    let Some(workspace) = cx.windows().into_iter().find_map(|window| {
        let window = window.downcast::<MultiWorkspace>()?;
        Some(window.read(cx).ok()?.workspace().clone())
    }) else {
        log::warn!(
            "{} terminal hosts run with no terminal, and no window to say so",
            hosts.len()
        );
        return;
    };
    let commands = hosts
        .iter()
        .map(|host| host.command_line.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let message = format!(
        "{} {commands}",
        tr(
            "Terminals left running by an earlier Solo couldn't be opened here, and still run:",
            "之前的 Solo 留下的终端仍在后台运行，但无法在这里打开：",
        )
    );
    let pipes: Vec<String> = hosts.into_iter().map(|host| host.pipe).collect();
    workspace.update(cx, |workspace, cx| {
        workspace.show_notification(NotificationId::unique::<LoneTerminalHosts>(), cx, |cx| {
            cx.new(|cx| {
                MessageNotification::new(message, cx)
                    .primary_message(tr("End Them", "结束它们"))
                    .primary_on_click(move |_, cx| {
                        let pipes = pipes.clone();
                        cx.background_spawn(async move {
                            for pipe in pipes {
                                terminal::end_running_host(&pipe).log_err();
                            }
                        })
                        .detach();
                        cx.emit(gpui::DismissEvent);
                    })
                    .secondary_message(tr("Keep Them Running", "保持运行"))
                    .secondary_on_click(|_, cx| cx.emit(gpui::DismissEvent))
            })
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saved(workspace_id: i64, paths: &[&str]) -> SavedHost {
        SavedHost {
            pipe: r"\\.\pipe\solo-terminal-1-2".to_string(),
            workspace_id: WorkspaceId::from_i64(workspace_id),
            paths: PathList::new(paths),
            identity_paths: None,
        }
    }

    fn open(database_id: i64, paths: &[&str]) -> Tab<()> {
        Tab {
            workspace: Some(()),
            database_id: Some(WorkspaceId::from_i64(database_id)),
            paths: PathList::new(paths),
        }
    }

    fn not_open(paths: &[&str]) -> Tab<()> {
        Tab {
            workspace: None,
            database_id: None,
            paths: PathList::new(paths),
        }
    }

    /// A host whose saved terminal is in a tab not open yet waits for that
    /// tab; it isn't opened in the current one.
    #[test]
    fn test_a_saved_terminal_waits_for_its_tab() {
        let tabs = [
            open(1, &["/repos/tcframework"]),
            not_open(&["/repos/solo"]),
            not_open(&["/repos/engine"]),
        ];
        assert_eq!(
            saved_tab(&saved(2, &["/repos/solo"]), &tabs),
            SavedTab::NotOpenYet
        );
        assert_eq!(
            saved_tab(&saved(3, &["/repos/engine"]), &tabs),
            SavedTab::NotOpenYet
        );
        // Its tab is open: there, by its workspace.
        assert_eq!(
            saved_tab(&saved(1, &["/repos/tcframework"]), &tabs),
            SavedTab::Open(0)
        );
        let opened = [open(1, &["/repos/tcframework"]), open(2, &["/repos/solo"])];
        assert_eq!(
            saved_tab(&saved(2, &["/repos/solo"]), &opened),
            SavedTab::Open(1)
        );
        // No tab has its workspace: opened by its directory.
        assert_eq!(
            saved_tab(&saved(4, &["/repos/gone"]), &tabs),
            SavedTab::None
        );
        // A linked worktree of a tab's project: that tab opens the main
        // repository, which doesn't restore it; opened in that tab.
        let worktree = SavedHost {
            identity_paths: Some(PathList::new(&["/repos/engine"])),
            ..saved(5, &["/worktrees/engine-fix"])
        };
        assert_eq!(saved_tab(&worktree, &tabs), SavedTab::Worktree(2));
    }
}
