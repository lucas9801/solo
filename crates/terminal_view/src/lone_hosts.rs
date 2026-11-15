//! SOLO: terminal hosts no terminal of this Solo uses. A host outlives Solo
//! when its terminal was busy as Solo went (quit, restarted for an update,
//! crashed), for the next Solo to restore the terminal and connect to it
//! again. When that doesn't happen (its workspace isn't restored, or its
//! saved terminal is gone), the host would run on unseen, with its agent or
//! build. Hosts list themselves in Solo's data folder (see
//! `terminal::running_hosts`); after restoring the session, Solo opens each
//! one no terminal connected to as a terminal in its first window, and
//! offers to end those it can't open.

use collections::HashSet;
use gpui::{App, AppContext as _, AsyncApp, Entity, WindowHandle};
use std::time::Duration;
use terminal::RunningHost;
use util::ResultExt as _;
use util::i18n::tr;
use workspace::notifications::NotificationId;
use workspace::notifications::simple_message_notification::MessageNotification;
use workspace::{MultiWorkspace, Workspace};

use crate::persistence::TerminalDb;
use crate::terminal_panel::TerminalPanel;

/// After the session is restored, its terminals still connect to their
/// hosts for a moment.
const FIRST_LOOK: Duration = Duration::from_secs(5);
/// Hosts a saved terminal refers to get longer: its workspace may still be
/// opening.
const SECOND_LOOK: Duration = Duration::from_secs(60);

struct LoneTerminalHosts;

/// Looks for hosts no terminal uses, once the session is restored.
pub fn look_for_lone_terminal_hosts(cx: &mut App) {
    let db = TerminalDb::global(cx);
    cx.spawn(async move |cx| {
        let mut seen = HashSet::default();
        cx.background_executor().timer(FIRST_LOOK).await;
        look(&db, false, &mut seen, cx).await;
        cx.background_executor().timer(SECOND_LOOK).await;
        look(&db, true, &mut seen, cx).await;
    })
    .detach();
}

async fn look(db: &TerminalDb, with_saved: bool, seen: &mut HashSet<String>, cx: &mut AsyncApp) {
    let saved: HashSet<String> = db
        .terminal_hosts()
        .log_err()
        .unwrap_or_default()
        .into_iter()
        .collect();
    let hosts = cx
        .background_spawn(async move { terminal::running_hosts() })
        .await;
    let lone: Vec<RunningHost> = hosts
        .into_iter()
        .filter(|host| {
            host.is_left_alone()
                && (with_saved || !saved.contains(&host.pipe))
                && !seen.contains(&host.pipe)
        })
        .collect();
    if lone.is_empty() {
        return;
    }
    let mut not_opened = Vec::new();
    for host in lone {
        seen.insert(host.pipe.clone());
        log::info!(
            "terminal host {} ({}) runs {} with no terminal, opening it",
            host.host_pid,
            host.pipe,
            host.command_line
        );
        if let Err(error) = open_as_terminal(&host, cx).await {
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

async fn open_as_terminal(host: &RunningHost, cx: &mut AsyncApp) -> anyhow::Result<()> {
    let (window, workspace) = cx
        .update(|cx| local_workspace(cx))
        .ok_or_else(|| anyhow::anyhow!("no window with a local project"))?;
    let env: collections::HashMap<String, String> = [
        (terminal::HOST_RECONNECT_ENV.to_string(), host.pipe.clone()),
        (
            terminal::HOST_RECONNECT_ONLY_ENV.to_string(),
            "1".to_string(),
        ),
    ]
    .into_iter()
    .collect();
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
