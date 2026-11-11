use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    rc::Rc,
    time::Duration,
};

use sysinfo::System;

use language::language_settings::{EditPredictionProvider, all_language_settings};

use client::proto;
use collections::HashSet;
use editor::{Editor, EditorEvent};
use gpui::{Action as _, Anchor, App, Entity, Subscription, Task, TaskExt, WeakEntity, actions};
use language::{BinaryStatus, BufferId, ServerHealth};
use lsp::{LanguageServerId, LanguageServerName, LanguageServerSelector};
use path::PathStyle;
use project::{
    LspStore, LspStoreEvent, Worktree, lsp_store::log_store::GlobalLogStore,
    project_settings::ProjectSettings, trusted_worktrees::TrustedWorktrees,
};
use settings::{Settings as _, SettingsStore};
use ui::{
    ContextMenu, ContextMenuEntry, Indicator, PopoverMenu, PopoverMenuHandle, Tooltip, prelude::*,
};

use util::{ResultExt, paths::PathExt, rel_path::RelPath};
use workspace::{StatusItemView, ToggleWorktreeSecurity, Workspace};

use crate::lsp_log_view;
use crate::server_resources::{
    self, ProcessTreeUsage, ServerActivity, choose_mode, describe_server, describe_usage,
    format_memory, mode_label, modes_for, server_activity,
};
use project::lsp_store::solo_server_modes;

actions!(
    lsp_tool,
    [
        /// Toggles the language server tool menu.
        ToggleMenu
    ]
);

pub struct LspButton {
    server_state: Entity<LanguageServerState>,
    popover_menu_handle: PopoverMenuHandle<ContextMenu>,
    lsp_menu: Option<Entity<ContextMenu>>,
    lsp_menu_refresh: Task<()>,
    _usage_sampler: Task<()>,
    _subscriptions: Vec<Subscription>,
}

struct LanguageServerState {
    items: Vec<LspMenuItem>,
    workspace: WeakEntity<Workspace>,
    lsp_store: WeakEntity<LspStore>,
    active_editor: Option<ActiveEditor>,
    language_servers: LanguageServers,
    /// SOLO: memory and processes per server process id, sampled in the
    /// background.
    server_usage: HashMap<u32, ProcessTreeUsage>,
}

impl std::fmt::Debug for LanguageServerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LanguageServerState")
            .field("items", &self.items)
            .field("workspace", &self.workspace)
            .field("lsp_store", &self.lsp_store)
            .field("active_editor", &self.active_editor)
            .field("language_servers", &self.language_servers)
            .finish_non_exhaustive()
    }
}

struct ActiveEditor {
    editor: WeakEntity<Editor>,
    _editor_subscription: Subscription,
    editor_buffers: HashSet<BufferId>,
}

impl std::fmt::Debug for ActiveEditor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActiveEditor")
            .field("editor", &self.editor)
            .field("editor_buffers", &self.editor_buffers)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Default, Clone)]
struct LanguageServers {
    health_statuses: HashMap<LanguageServerId, LanguageServerHealthStatus>,
    binary_statuses: HashMap<LanguageServerName, LanguageServerBinaryStatus>,
    servers_per_buffer_abs_path: HashMap<PathBuf, ServersForPath>,
}

#[derive(Debug, Clone)]
struct ServersForPath {
    servers: HashMap<LanguageServerId, Option<LanguageServerName>>,
    worktree: Option<WeakEntity<Worktree>>,
}

#[derive(Debug, Clone)]
struct LanguageServerHealthStatus {
    name: LanguageServerName,
    health: Option<(Option<SharedString>, ServerHealth)>,
}

#[derive(Debug, Clone)]
struct LanguageServerBinaryStatus {
    status: BinaryStatus,
    message: Option<SharedString>,
}

#[derive(Debug, Clone)]
struct ServerInfo {
    name: LanguageServerName,
    id: LanguageServerId,
    health: Option<ServerHealth>,
    binary_status: Option<LanguageServerBinaryStatus>,
    message: Option<SharedString>,
}

#[derive(Default, Clone)]
struct ServerMetadata {
    server_version: Option<SharedString>,
    binary_display_path: Option<SharedString>,
    process_id: Option<u32>,
    /// SOLO: the root of the project the server runs for, and what it is
    /// doing.
    project_root: Option<PathBuf>,
    activity: Option<ServerActivity>,
}

impl ServerInfo {
    fn server_selector(&self) -> LanguageServerSelector {
        LanguageServerSelector::Id(self.id)
    }

    fn can_stop(&self) -> bool {
        self.binary_status.as_ref().is_none_or(|status| {
            matches!(status.status, BinaryStatus::None | BinaryStatus::Starting)
        })
    }
}

impl LanguageServerHealthStatus {
    fn health(&self) -> Option<ServerHealth> {
        self.health.as_ref().map(|(_, health)| *health)
    }

    fn message(&self) -> Option<SharedString> {
        self.health
            .as_ref()
            .and_then(|(message, _)| message.clone())
    }
}

impl LanguageServerState {
    fn fill_menu(&self, mut menu: ContextMenu, cx: &mut Context<Self>) -> ContextMenu {
        let lsp_logs = cx
            .try_global::<GlobalLogStore>()
            .map(|lsp_logs| lsp_logs.0.clone());
        let Some(lsp_logs) = lsp_logs else {
            return menu;
        };

        let is_restricted = self
            .workspace
            .upgrade()
            .map(|workspace| {
                let worktree_store = workspace.read(cx).project().read(cx).worktree_store();
                TrustedWorktrees::has_restricted_worktrees(&worktree_store, cx)
            })
            .unwrap_or(false);

        if is_restricted {
            menu = menu.custom_entry(
                move |_window, _cx| {
                    v_flex()
                        .child(
                            h_flex()
                                .gap_1()
                                .child(
                                    Icon::new(IconName::Warning)
                                        .color(Color::Warning)
                                        .size(IconSize::XSmall),
                                )
                                .child(
                                    Label::new("Project is in Restricted Mode")
                                        .size(LabelSize::Small),
                                ),
                        )
                        .child(
                            Label::new("Language Servers can't run until you trust this project.")
                                .size(LabelSize::Small)
                                .color(Color::Muted),
                        )
                        .into_any_element()
                },
                move |window, cx| {
                    window.dispatch_action(ToggleWorktreeSecurity.boxed_clone(), cx);
                },
            );
        }

        let path_style = self
            .workspace
            .upgrade()
            .map(|workspace| workspace.read(cx).path_style(cx))
            .unwrap_or(PathStyle::local());

        let server_metadata =
            self.lsp_store
                .update(cx, |lsp_store, cx| {
                    let worktree_store = lsp_store.worktree_store();
                    lsp_store
                        .language_server_statuses()
                        .map(|(server_id, status)| {
                            (
                                server_id,
                                ServerMetadata {
                                    server_version: status.server_readable_version.clone(),
                                    binary_display_path: status.binary.as_ref().map(|binary| {
                                        tooltip_for_server_binary(binary, path_style)
                                    }),
                                    process_id: status.process_id,
                                    project_root: status
                                        .worktree
                                        .and_then(|worktree_id| {
                                            worktree_store.read(cx).worktree_for_id(worktree_id, cx)
                                        })
                                        .map(|worktree| worktree.read(cx).abs_path().to_path_buf()),
                                    activity: Some(server_activity(status.pending_work.values())),
                                },
                            )
                        })
                        .collect::<HashMap<_, _>>()
                })
                .unwrap_or_default();

        let mut first_button_encountered = false;
        for item in &self.items {
            if let LspMenuItem::ToggleServersButton { restart } = item {
                let label = if *restart {
                    "Restart All Servers"
                } else {
                    "Stop All Servers"
                };

                let restart = *restart;

                let button = ContextMenuEntry::new(label).handler({
                    let state = cx.entity();
                    move |_, cx| {
                        let lsp_store = state.read(cx).lsp_store.clone();
                        lsp_store
                            .update(cx, |lsp_store, cx| {
                                if restart {
                                    lsp_store.restart_all_language_servers(cx);
                                } else {
                                    lsp_store.stop_all_language_servers(cx);
                                }
                            })
                            .ok();
                    }
                });

                if !first_button_encountered {
                    menu = menu.separator();
                    first_button_encountered = true;
                }

                menu = menu.item(button);
                continue;
            } else if let LspMenuItem::Header { header, separator } = item {
                menu = menu
                    .when(*separator, |menu| menu.separator())
                    .when_some(header.as_ref(), |menu, header| menu.header(header));
                continue;
            }

            let Some(server_info) = item.server_info() else {
                continue;
            };
            let server_selector = server_info.server_selector();
            let is_remote = self
                .lsp_store
                .update(cx, |lsp_store, _| lsp_store.as_remote().is_some())
                .unwrap_or(false);
            let has_logs = is_remote
                || self.workspace.upgrade().is_some_and(|workspace| {
                    let project = workspace.read(cx).project();
                    lsp_logs.read(cx).has_server_logs(
                        &server_selector,
                        &project.downgrade(),
                        &self.lsp_store,
                    )
                });

            let (status_color, status_label) = server_info
                .binary_status
                .as_ref()
                .and_then(|binary_status| match binary_status.status {
                    BinaryStatus::None => None,
                    BinaryStatus::CheckingForUpdate
                    | BinaryStatus::Downloading
                    | BinaryStatus::Starting => Some((Color::Modified, "Starting…")),
                    BinaryStatus::Stopping | BinaryStatus::Stopped => {
                        Some((Color::Disabled, "Stopped"))
                    }
                    BinaryStatus::Failed { .. } => Some((Color::Error, "Error")),
                })
                .or_else(|| {
                    Some(match server_info.health? {
                        ServerHealth::Ok => (Color::Success, "Running"),
                        ServerHealth::Warning => (Color::Warning, "Warning"),
                        ServerHealth::Error => (Color::Error, "Error"),
                    })
                })
                .unwrap_or((Color::Success, "Running"));

            let message = server_info
                .message
                .as_ref()
                .or_else(|| server_info.binary_status.as_ref()?.message.as_ref())
                .cloned();

            let ServerMetadata {
                server_version,
                binary_display_path,
                process_id,
                project_root,
                activity,
            } = server_metadata
                .get(&server_info.id)
                .cloned()
                .unwrap_or_default();

            let server_message = message.clone();

            let submenu_server_name = server_info.name.clone();
            let submenu_server_info = server_info.clone();

            menu = menu.submenu_with_colored_icon(
                server_info.name.0.clone(),
                IconName::Circle,
                status_color,
                {
                    let lsp_logs = lsp_logs.clone();
                    let message = message.clone();
                    let server_selector = server_selector.clone();
                    let workspace = self.workspace.clone();
                    let lsp_store = self.lsp_store.clone();
                    let state = cx.entity().downgrade();
                    let can_stop = submenu_server_info.can_stop();
                    let project_root = project_root.clone().filter(|_| !is_remote);
                    let activity = activity.clone();

                    move |menu, _window, cx| {
                        let mut submenu = menu;

                        if let Some(ref message) = message {
                            let workspace_for_message = workspace.clone();
                            let message_for_handler = message.clone();
                            let server_name_for_message = submenu_server_name.clone();
                            submenu = submenu.entry("View Message", None, move |window, cx| {
                                let Some(create_buffer) = workspace_for_message
                                    .update(cx, |workspace, cx| {
                                        workspace.project().update(cx, |project, cx| {
                                            project.create_buffer(None, false, cx)
                                        })
                                    })
                                    .ok()
                                else {
                                    return;
                                };

                                let window_handle = window.window_handle();
                                let workspace = workspace_for_message.clone();
                                let message = message_for_handler.clone();
                                let server_name = server_name_for_message.clone();
                                cx.spawn(async move |cx| {
                                    let buffer = create_buffer.await?;
                                    buffer.update(cx, |buffer, cx| {
                                        buffer.edit(
                                            [(
                                                0..0,
                                                format!(
                                                    "Language server {server_name}:\n\n{message}"
                                                ),
                                            )],
                                            None,
                                            cx,
                                        );
                                        buffer.set_capability(language::Capability::ReadOnly, cx);
                                    });

                                    workspace.update(cx, |workspace, cx| {
                                        window_handle.update(cx, |_, window, cx| {
                                            workspace.add_item_to_active_pane(
                                                Box::new(cx.new(|cx| {
                                                    let mut editor = Editor::for_buffer(
                                                        buffer, None, window, cx,
                                                    );
                                                    editor.set_read_only(true);
                                                    editor
                                                })),
                                                None,
                                                true,
                                                window,
                                                cx,
                                            );
                                        })
                                    })??;

                                    anyhow::Ok(())
                                })
                                .detach();
                            });
                        }

                        if has_logs {
                            let lsp_logs_for_debug = lsp_logs.clone();
                            let workspace_for_debug = workspace.clone();
                            let server_selector_for_debug = server_selector.clone();
                            submenu = submenu.entry("View Logs", None, move |window, cx| {
                                lsp_log_view::open(
                                    &lsp_logs_for_debug,
                                    workspace_for_debug.clone(),
                                    server_selector_for_debug.clone(),
                                    window,
                                    cx,
                                );
                            });
                        }

                        let state_for_restart = state.clone();
                        let workspace_for_restart = workspace.clone();
                        let lsp_store_for_restart = lsp_store.clone();
                        let server_name_for_restart = submenu_server_name.clone();
                        submenu = submenu.entry("Restart Server", None, move |_window, cx| {
                            let Some(workspace) = workspace_for_restart.upgrade() else {
                                return;
                            };

                            let project = workspace.read(cx).project().clone();
                            let path_style = project.read(cx).path_style(cx);
                            let buffer_store = project.read(cx).buffer_store().clone();

                            let buffers = state_for_restart
                                .update(cx, |state, cx| {
                                    let server_buffers = state
                                        .language_servers
                                        .servers_per_buffer_abs_path
                                        .iter()
                                        .filter_map(|(abs_path, servers)| {
                                            // Check if this server is associated with this path
                                            let has_server = servers.servers.values().any(|name| {
                                                name.as_ref() == Some(&server_name_for_restart)
                                            });

                                            if !has_server {
                                                return None;
                                            }

                                            let worktree = servers.worktree.as_ref()?.upgrade()?;
                                            let worktree_ref = worktree.read(cx);
                                            let relative_path = abs_path
                                                .strip_prefix(&worktree_ref.abs_path())
                                                .ok()?;
                                            let relative_path =
                                                RelPath::new(relative_path, path_style)
                                                    .log_err()?;
                                            let entry =
                                                worktree_ref.entry_for_path(&relative_path)?;
                                            let project_path =
                                                project.read(cx).path_for_entry(entry.id, cx)?;

                                            buffer_store.read(cx).get_by_path(&project_path)
                                        })
                                        .collect::<Vec<_>>();

                                    if server_buffers.is_empty() {
                                        state
                                            .language_servers
                                            .servers_per_buffer_abs_path
                                            .iter()
                                            .filter_map(|(abs_path, servers)| {
                                                let worktree =
                                                    servers.worktree.as_ref()?.upgrade()?.read(cx);
                                                let relative_path = abs_path
                                                    .strip_prefix(&worktree.abs_path())
                                                    .ok()?;
                                                let relative_path =
                                                    RelPath::new(relative_path, path_style)
                                                        .log_err()?;
                                                let entry =
                                                    worktree.entry_for_path(&relative_path)?;
                                                let project_path = project
                                                    .read(cx)
                                                    .path_for_entry(entry.id, cx)?;
                                                buffer_store.read(cx).get_by_path(&project_path)
                                            })
                                            .collect()
                                    } else {
                                        server_buffers
                                    }
                                })
                                .unwrap_or_default();

                            if !buffers.is_empty() {
                                lsp_store_for_restart
                                    .update(cx, |lsp_store, cx| {
                                        lsp_store.restart_language_servers_for_buffers(
                                            buffers,
                                            HashSet::from_iter([LanguageServerSelector::Name(
                                                server_name_for_restart.clone(),
                                            )]),
                                            cx,
                                        );
                                    })
                                    .ok();
                            }
                        });

                        if can_stop {
                            let lsp_store_for_stop = lsp_store.clone();
                            let server_selector_for_stop = server_selector.clone();

                            submenu = submenu.entry("Stop Server", None, move |_window, cx| {
                                lsp_store_for_stop
                                    .update(cx, |lsp_store, cx| {
                                        lsp_store
                                            .stop_language_servers_for_buffers(
                                                Vec::new(),
                                                HashSet::from_iter([
                                                    server_selector_for_stop.clone()
                                                ]),
                                                cx,
                                            )
                                            .detach_and_log_err(cx);
                                    })
                                    .ok();
                            });
                        }

                        // SOLO: how much this server analyzes in this
                        // project, remembered across restarts.
                        if let Some(project_root) = project_root.clone() {
                            let server_name = submenu_server_name.clone();
                            let current =
                                solo_server_modes::mode_for(&project_root, &server_name, cx);
                            submenu = submenu
                                .separator()
                                .header(util::i18n::tr("In This Project", "在本项目"));
                            for mode in modes_for(&server_name) {
                                let project_root = project_root.clone();
                                let server_name = server_name.clone();
                                submenu = submenu.toggleable_entry(
                                    mode_label(mode),
                                    mode == current,
                                    IconPosition::Start,
                                    None,
                                    move |_, cx| {
                                        choose_mode(
                                            project_root.clone(),
                                            server_name.clone(),
                                            mode,
                                            cx,
                                        );
                                    },
                                );
                            }
                        }

                        submenu = submenu.separator().custom_row({
                            let binary_display_path = binary_display_path.clone();
                            let server_version = server_version.clone();
                            let server_message = server_message.clone();
                            let state = state.clone();
                            let activity = activity.clone();
                            move |_, cx| {
                                // SOLO: memory includes the server's child
                                // processes (cargo and rustc under
                                // rust-analyzer), sampled in the background.
                                let usage = process_id.and_then(|pid| {
                                    state.upgrade()?.read(cx).server_usage.get(&pid).copied()
                                });
                                let memory_label =
                                    (usage.is_some() || activity.is_some()).then(|| {
                                        describe_usage(
                                            activity.as_ref().unwrap_or(&ServerActivity::Idle),
                                            usage,
                                        )
                                    });

                                let version_label =
                                    server_version.as_ref().map(|v| format!("v{}", v.as_ref()));

                                let separator_color =
                                    cx.theme().colors().icon_disabled.opacity(0.8);

                                v_flex()
                                    .id("metadata-container")
                                    .gap_1()
                                    .when_some(server_message.as_ref(), |this, _| {
                                        this.w(rems_from_px(240_f32))
                                    })
                                    .child(
                                        h_flex()
                                            .ml_neg_1()
                                            .gap_1()
                                            .child(
                                                Icon::new(IconName::Circle)
                                                    .color(status_color)
                                                    .size(IconSize::Small),
                                            )
                                            .child(
                                                Label::new(status_label)
                                                    .size(LabelSize::Small)
                                                    .color(Color::Muted),
                                            )
                                            .when_some(version_label.as_ref(), |row, version| {
                                                row.child(
                                                    Icon::new(IconName::Dash)
                                                        .color(Color::Custom(separator_color))
                                                        .size(IconSize::XSmall),
                                                )
                                                .child(
                                                    Label::new(version)
                                                        .size(LabelSize::Small)
                                                        .color(Color::Muted),
                                                )
                                            })
                                            .when_some(memory_label.as_ref(), |row, memory| {
                                                row.child(
                                                    Icon::new(IconName::Dash)
                                                        .color(Color::Custom(separator_color))
                                                        .size(IconSize::XSmall),
                                                )
                                                .child(
                                                    Label::new(memory)
                                                        .size(LabelSize::Small)
                                                        .color(Color::Muted),
                                                )
                                            }),
                                    )
                                    .when_some(server_message.clone(), |container, message| {
                                        container.child(
                                            Label::new(message)
                                                .color(Color::Muted)
                                                .size(LabelSize::Small),
                                        )
                                    })
                                    .when_some(binary_display_path.clone(), |el, path| {
                                        el.tooltip(Tooltip::text(path))
                                    })
                                    .into_any_element()
                            }
                        });

                        submenu
                    }
                },
            );
        }

        // SOLO: servers turned off in this project, to turn on again.
        let is_remote = self
            .lsp_store
            .update(cx, |lsp_store, _| lsp_store.as_remote().is_some())
            .unwrap_or(true);
        let project_roots: Vec<PathBuf> = if is_remote {
            Vec::new()
        } else {
            self.workspace
                .upgrade()
                .map(|workspace| {
                    workspace
                        .read(cx)
                        .project()
                        .read(cx)
                        .visible_worktrees(cx)
                        .map(|worktree| worktree.read(cx).abs_path().to_path_buf())
                        .collect()
                })
                .unwrap_or_default()
        };
        let mut header_added = false;
        for project_root in project_roots {
            for server_name in server_resources::servers_off_in(&project_root, cx) {
                if !header_added {
                    menu = menu
                        .separator()
                        .header(util::i18n::tr("Off in This Project", "已在本项目关闭"));
                    header_added = true;
                }
                let label = if util::i18n::is_chinese() {
                    format!("开启 {server_name}")
                } else {
                    format!("Turn On {server_name}")
                };
                let project_root = project_root.clone();
                menu = menu.entry(label, None, move |_, cx| {
                    choose_mode(
                        project_root.clone(),
                        server_name.clone(),
                        solo_server_modes::AnalysisMode::Full,
                        cx,
                    );
                });
            }
        }
        menu
    }
}

fn tooltip_for_server_binary(
    server_binary: &lsp::LanguageServerBinary,
    path_style: PathStyle,
) -> SharedString {
    let runtime = path_style.file_name(&server_binary.path).and_then(|name| {
        ["node", "python"]
            .into_iter()
            .find(|runtime| name.to_string_lossy().starts_with(runtime))
    });

    let target_path = runtime
        .and_then(|_runtime| {
            server_binary
                .arguments
                .iter()
                .find(|arg| !arg.to_string_lossy().starts_with('-'))
        })
        .map(Path::new)
        .unwrap_or(&server_binary.path);

    let display_path = path_style
        .normalize_lexically(&target_path.compact())
        .to_string_lossy()
        .into_owned();

    match runtime {
        Some(runtime) => format!("{display_path} ({runtime})").into(),
        None => display_path.into(),
    }
}

impl LanguageServers {
    fn update_binary_status(
        &mut self,
        binary_status: BinaryStatus,
        message: Option<&str>,
        name: LanguageServerName,
    ) {
        let binary_status_message = message.map(SharedString::new);
        if matches!(
            binary_status,
            BinaryStatus::Stopped | BinaryStatus::Failed { .. }
        ) {
            self.health_statuses.retain(|_, server| server.name != name);
        }
        self.binary_statuses.insert(
            name,
            LanguageServerBinaryStatus {
                status: binary_status,
                message: binary_status_message,
            },
        );
    }

    fn update_server_health(
        &mut self,
        id: LanguageServerId,
        health: ServerHealth,
        message: Option<&str>,
        name: Option<LanguageServerName>,
    ) {
        if let Some(state) = self.health_statuses.get_mut(&id) {
            state.health = Some((message.map(SharedString::new), health));
            if let Some(name) = name {
                state.name = name;
            }
        } else if let Some(name) = name {
            self.health_statuses.insert(
                id,
                LanguageServerHealthStatus {
                    health: Some((message.map(SharedString::new), health)),
                    name,
                },
            );
        }
    }

    fn is_empty(&self) -> bool {
        self.binary_statuses.is_empty() && self.health_statuses.is_empty()
    }

    /// Drop all id-keyed state for a server that has been removed (stopped or
    /// reaching end-of-life via restart). `binary_statuses` is intentionally
    /// preserved — it is keyed by name and shared across restart cycles to
    /// drive the "Downloading… → Starting…" status UX.
    fn remove_server(&mut self, server_id: LanguageServerId) {
        self.health_statuses.remove(&server_id);
        self.servers_per_buffer_abs_path
            .retain(|_, servers_for_path| {
                servers_for_path.servers.remove(&server_id);
                !servers_for_path.servers.is_empty()
            });
    }
}

#[derive(Debug)]
enum ServerData<'a> {
    WithHealthCheck {
        server_id: LanguageServerId,
        health: &'a LanguageServerHealthStatus,
        binary_status: Option<&'a LanguageServerBinaryStatus>,
    },
    WithBinaryStatus {
        server_id: LanguageServerId,
        server_name: &'a LanguageServerName,
        binary_status: &'a LanguageServerBinaryStatus,
    },
}

#[derive(Debug)]
enum LspMenuItem {
    WithHealthCheck {
        server_id: LanguageServerId,
        health: LanguageServerHealthStatus,
        binary_status: Option<LanguageServerBinaryStatus>,
    },
    WithBinaryStatus {
        server_id: LanguageServerId,
        server_name: LanguageServerName,
        binary_status: LanguageServerBinaryStatus,
    },
    ToggleServersButton {
        restart: bool,
    },
    Header {
        header: Option<SharedString>,
        separator: bool,
    },
}

impl LspMenuItem {
    fn server_info(&self) -> Option<ServerInfo> {
        match self {
            Self::Header { .. } => None,
            Self::ToggleServersButton { .. } => None,
            Self::WithHealthCheck {
                server_id,
                health,
                binary_status,
                ..
            } => Some(ServerInfo {
                name: health.name.clone(),
                id: *server_id,
                health: health.health(),
                binary_status: binary_status.clone(),
                message: health.message(),
            }),
            Self::WithBinaryStatus {
                server_id,
                server_name,
                binary_status,
                ..
            } => Some(ServerInfo {
                name: server_name.clone(),
                id: *server_id,
                health: None,
                binary_status: Some(binary_status.clone()),
                message: binary_status.message.clone(),
            }),
        }
    }
}

impl ServerData<'_> {
    fn into_lsp_item(self) -> LspMenuItem {
        match self {
            Self::WithHealthCheck {
                server_id,
                health,
                binary_status,
                ..
            } => LspMenuItem::WithHealthCheck {
                server_id,
                health: health.clone(),
                binary_status: binary_status.cloned(),
            },
            Self::WithBinaryStatus {
                server_id,
                server_name,
                binary_status,
                ..
            } => LspMenuItem::WithBinaryStatus {
                server_id,
                server_name: server_name.clone(),
                binary_status: binary_status.clone(),
            },
        }
    }
}

impl LspButton {
    pub fn new(
        workspace: &Workspace,
        popover_menu_handle: PopoverMenuHandle<ContextMenu>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let settings_subscription =
            cx.observe_global_in::<SettingsStore>(window, move |lsp_button, window, cx| {
                if ProjectSettings::get_global(cx).global_lsp_settings.button {
                    if lsp_button.lsp_menu.is_none() {
                        lsp_button.refresh_lsp_menu(true, window, cx);
                    }
                } else if lsp_button.lsp_menu.take().is_some() {
                    cx.notify();
                }
            });

        let lsp_store = workspace.project().read(cx).lsp_store();
        let mut language_servers = LanguageServers::default();
        for (_, status) in lsp_store.read(cx).language_server_statuses() {
            language_servers.binary_statuses.insert(
                status.name.clone(),
                LanguageServerBinaryStatus {
                    status: BinaryStatus::None,
                    message: None,
                },
            );
        }

        let lsp_store_subscription =
            cx.subscribe_in(&lsp_store, window, |lsp_button, _, e, window, cx| {
                lsp_button.on_lsp_store_event(e, window, cx)
            });

        let server_state = cx.new(|_| LanguageServerState {
            workspace: workspace.weak_handle(),
            items: Vec::new(),
            lsp_store: lsp_store.downgrade(),
            active_editor: None,
            language_servers,
            server_usage: HashMap::new(),
        });

        let mut lsp_button = Self {
            server_state: server_state.clone(),
            popover_menu_handle,
            lsp_menu: None,
            lsp_menu_refresh: Task::ready(()),
            _usage_sampler: Self::sample_usage(server_state, cx),
            _subscriptions: vec![settings_subscription, lsp_store_subscription],
        };
        let is_restricted = TrustedWorktrees::has_restricted_worktrees(
            &workspace.project().read(cx).worktree_store(),
            cx,
        );

        if is_restricted
            || !lsp_button
                .server_state
                .read(cx)
                .language_servers
                .binary_statuses
                .is_empty()
        {
            lsp_button.refresh_lsp_menu(true, window, cx);
        }

        lsp_button
    }

    /// SOLO: samples the memory and processes of the running servers (with
    /// their children) every few seconds, on a background thread.
    fn sample_usage(server_state: Entity<LanguageServerState>, cx: &mut Context<Self>) -> Task<()> {
        const INTERVAL: Duration = Duration::from_secs(4);
        cx.spawn(async move |lsp_button, cx| {
            let mut system = System::new();
            loop {
                let roots = server_state.update(cx, |state, cx| {
                    let Some(lsp_store) = state.lsp_store.upgrade() else {
                        return Vec::new();
                    };
                    let lsp_store = lsp_store.read(cx);
                    if lsp_store.as_remote().is_some() {
                        return Vec::new();
                    }
                    lsp_store
                        .language_server_statuses()
                        .filter_map(|(_, status)| status.process_id)
                        .collect::<Vec<_>>()
                });
                let usage = if roots.is_empty() {
                    HashMap::new()
                } else {
                    let (returned_system, usage) = cx
                        .background_spawn(async move {
                            let usage = server_resources::sample_process_trees(&mut system, &roots);
                            (system, usage)
                        })
                        .await;
                    system = returned_system;
                    usage.into_iter().collect()
                };
                let changed = server_state.update(cx, |state, _| {
                    let changed = state.server_usage != usage;
                    state.server_usage = usage;
                    changed
                });
                if changed {
                    if lsp_button.update(cx, |_, cx| cx.notify()).is_err() {
                        return;
                    }
                }
                cx.background_executor().timer(INTERVAL).await;
            }
        })
    }

    fn on_lsp_store_event(
        &mut self,
        e: &LspStoreEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // SOLO: a heavy server starting in a very large project asks once
        // how much it should analyze.
        if let LspStoreEvent::LanguageServerAdded(_, name, Some(worktree_id)) = e {
            let workspace = self.server_state.read(cx).workspace.clone();
            let worktree = workspace.upgrade().and_then(|workspace| {
                workspace
                    .read(cx)
                    .project()
                    .read(cx)
                    .worktree_for_id(*worktree_id, cx)
            });
            if let Some(worktree) = worktree {
                server_resources::maybe_ask_about_large_project(
                    workspace,
                    worktree,
                    name.clone(),
                    cx,
                );
            }
        }
        if self.lsp_menu.is_none() {
            return;
        };
        let mut updated = false;

        // TODO `LspStore` is global and reports status from all language servers, even from the other windows.
        match e {
            LspStoreEvent::LanguageServerUpdate {
                language_server_id,
                name,
                message: proto::update_language_server::Variant::StatusUpdate(status_update),
            } => match &status_update.status {
                Some(proto::status_update::Status::Binary(binary_status)) => {
                    let Some(name) = name.as_ref() else {
                        return;
                    };
                    if let Some(binary_status) =
                        proto::ServerBinaryStatus::try_from(*binary_status).ok()
                    {
                        let binary_status = match binary_status {
                            proto::ServerBinaryStatus::None => BinaryStatus::None,
                            proto::ServerBinaryStatus::CheckingForUpdate => {
                                BinaryStatus::CheckingForUpdate
                            }
                            proto::ServerBinaryStatus::Downloading => BinaryStatus::Downloading,
                            proto::ServerBinaryStatus::Starting => BinaryStatus::Starting,
                            proto::ServerBinaryStatus::Stopping => BinaryStatus::Stopping,
                            proto::ServerBinaryStatus::Stopped => BinaryStatus::Stopped,
                            proto::ServerBinaryStatus::Failed => {
                                let Some(error) = status_update.message.clone() else {
                                    return;
                                };
                                BinaryStatus::Failed { error }
                            }
                        };
                        self.server_state.update(cx, |state, _| {
                            state.language_servers.update_binary_status(
                                binary_status,
                                status_update.message.as_deref(),
                                name.clone(),
                            );
                        });
                        updated = true;
                    };
                }
                Some(proto::status_update::Status::Health(health_status)) => {
                    if let Some(health) = proto::ServerHealth::try_from(*health_status).ok() {
                        let health = match health {
                            proto::ServerHealth::Ok => ServerHealth::Ok,
                            proto::ServerHealth::Warning => ServerHealth::Warning,
                            proto::ServerHealth::Error => ServerHealth::Error,
                        };
                        self.server_state.update(cx, |state, _| {
                            state.language_servers.update_server_health(
                                *language_server_id,
                                health,
                                status_update.message.as_deref(),
                                name.clone(),
                            );
                        });
                        updated = true;
                    }
                }
                None => {}
            },
            LspStoreEvent::LanguageServerUpdate {
                language_server_id,
                name,
                message: proto::update_language_server::Variant::RegisteredForBuffer(update),
                ..
            } => {
                self.server_state.update(cx, |state, cx| {
                    let Ok(worktree) = state.workspace.update(cx, |workspace, cx| {
                        workspace
                            .project()
                            .read(cx)
                            .find_worktree(Path::new(&update.buffer_abs_path), cx)
                            .map(|(worktree, _)| worktree.downgrade())
                    }) else {
                        return;
                    };
                    let entry = state
                        .language_servers
                        .servers_per_buffer_abs_path
                        .entry(PathBuf::from(&update.buffer_abs_path))
                        .or_insert_with(|| ServersForPath {
                            servers: HashMap::default(),
                            worktree: worktree.clone(),
                        });
                    entry.servers.insert(*language_server_id, name.clone());
                    if worktree.is_some() {
                        entry.worktree = worktree;
                    }
                });
                updated = true;
            }
            LspStoreEvent::LanguageServerRemoved(server_id) => {
                self.server_state.update(cx, |state, _| {
                    state.language_servers.remove_server(*server_id);
                });
                updated = true;
            }
            _ => {}
        };

        if updated {
            self.refresh_lsp_menu(false, window, cx);
        }
    }

    fn regenerate_items(&mut self, cx: &mut App) {
        self.server_state.update(cx, |state, cx| {
            let active_worktrees = state
                .active_editor
                .as_ref()
                .into_iter()
                .flat_map(|active_editor| {
                    active_editor
                        .editor
                        .upgrade()
                        .into_iter()
                        .flat_map(|active_editor| {
                            active_editor
                                .read(cx)
                                .buffer()
                                .read(cx)
                                .all_buffers()
                                .into_iter()
                                .filter_map(|buffer| {
                                    project::File::from_dyn(buffer.read(cx).file())
                                })
                                .map(|buffer_file| buffer_file.worktree.clone())
                        })
                })
                .collect::<HashSet<_>>();

            let mut server_ids_to_worktrees =
                HashMap::<LanguageServerId, Entity<Worktree>>::default();
            let mut server_names_to_worktrees = HashMap::<
                LanguageServerName,
                HashSet<(Entity<Worktree>, LanguageServerId)>,
            >::default();
            for servers_for_path in state.language_servers.servers_per_buffer_abs_path.values() {
                if let Some(worktree) = servers_for_path
                    .worktree
                    .as_ref()
                    .and_then(|worktree| worktree.upgrade())
                {
                    for (server_id, server_name) in &servers_for_path.servers {
                        server_ids_to_worktrees.insert(*server_id, worktree.clone());
                        if let Some(server_name) = server_name {
                            server_names_to_worktrees
                                .entry(server_name.clone())
                                .or_default()
                                .insert((worktree.clone(), *server_id));
                        }
                    }
                }
            }
            state
                .lsp_store
                .update(cx, |lsp_store, cx| {
                    for (server_id, status) in lsp_store.language_server_statuses() {
                        if let Some(worktree) = status.worktree.and_then(|worktree_id| {
                            lsp_store
                                .worktree_store()
                                .read(cx)
                                .worktree_for_id(worktree_id, cx)
                        }) {
                            server_ids_to_worktrees.insert(server_id, worktree.clone());
                            server_names_to_worktrees
                                .entry(status.name.clone())
                                .or_default()
                                .insert((worktree, server_id));
                        }
                    }
                })
                .ok();

            let mut servers_per_worktree = BTreeMap::<SharedString, Vec<ServerData>>::new();
            let mut servers_with_health_checks = HashSet::default();

            for (server_id, health) in &state.language_servers.health_statuses {
                let worktree = server_ids_to_worktrees.get(server_id).or_else(|| {
                    let worktrees = server_names_to_worktrees.get(&health.name)?;
                    worktrees
                        .iter()
                        .find(|(worktree, _)| active_worktrees.contains(worktree))
                        .or_else(|| worktrees.iter().next())
                        .map(|(worktree, _)| worktree)
                });
                servers_with_health_checks.insert(&health.name);
                let worktree_name =
                    worktree.map(|worktree| SharedString::new(worktree.read(cx).root_name_str()));

                let binary_status = state.language_servers.binary_statuses.get(&health.name);
                let server_data = ServerData::WithHealthCheck {
                    server_id: *server_id,
                    health,
                    binary_status,
                };
                if let Some(worktree_name) = worktree_name {
                    servers_per_worktree
                        .entry(worktree_name.clone())
                        .or_default()
                        .push(server_data);
                }
            }

            let mut can_stop_all = !state.language_servers.health_statuses.is_empty();
            let mut can_restart_all = state.language_servers.health_statuses.is_empty();
            for (server_name, binary_status) in state
                .language_servers
                .binary_statuses
                .iter()
                .filter(|(name, _)| !servers_with_health_checks.contains(name))
            {
                match binary_status.status {
                    BinaryStatus::None => {
                        can_restart_all = false;
                        can_stop_all |= true;
                    }
                    BinaryStatus::CheckingForUpdate => {
                        can_restart_all = false;
                        can_stop_all = false;
                    }
                    BinaryStatus::Downloading => {
                        can_restart_all = false;
                        can_stop_all = false;
                    }
                    BinaryStatus::Starting => {
                        can_restart_all = false;
                        can_stop_all = false;
                    }
                    BinaryStatus::Stopping => {
                        can_restart_all = false;
                        can_stop_all = false;
                    }
                    BinaryStatus::Stopped => {}
                    BinaryStatus::Failed { .. } => {}
                }

                if let Some(worktrees_for_name) = server_names_to_worktrees.get(server_name)
                    && let Some((worktree, server_id)) = worktrees_for_name
                        .iter()
                        .find(|(worktree, _)| active_worktrees.contains(worktree))
                        .or_else(|| worktrees_for_name.iter().next())
                {
                    let worktree_name = SharedString::new(worktree.read(cx).root_name_str());
                    servers_per_worktree
                        .entry(worktree_name.clone())
                        .or_default()
                        .push(ServerData::WithBinaryStatus {
                            server_name,
                            binary_status,
                            server_id: *server_id,
                        });
                }
            }

            let mut new_lsp_items = Vec::with_capacity(servers_per_worktree.len() + 1);
            for (worktree_name, worktree_servers) in servers_per_worktree {
                if worktree_servers.is_empty() {
                    continue;
                }
                new_lsp_items.push(LspMenuItem::Header {
                    header: Some(worktree_name),
                    separator: false,
                });
                new_lsp_items.extend(worktree_servers.into_iter().map(ServerData::into_lsp_item));
            }
            if !new_lsp_items.is_empty() {
                if can_stop_all {
                    new_lsp_items.push(LspMenuItem::ToggleServersButton { restart: true });
                    new_lsp_items.push(LspMenuItem::ToggleServersButton { restart: false });
                } else if can_restart_all {
                    new_lsp_items.push(LspMenuItem::ToggleServersButton { restart: true });
                }
            }

            state.items = new_lsp_items;
        });
    }

    fn refresh_lsp_menu(
        &mut self,
        create_if_empty: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if create_if_empty || self.lsp_menu.is_some() {
            let state = self.server_state.clone();
            self.lsp_menu_refresh = cx.spawn_in(window, async move |lsp_button, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(30))
                    .await;
                lsp_button
                    .update_in(cx, |lsp_button, window, cx| {
                        lsp_button.regenerate_items(cx);
                        let menu = ContextMenu::build(window, cx, |menu, _, cx| {
                            state.update(cx, |state, cx| state.fill_menu(menu, cx))
                        });
                        lsp_button.lsp_menu = Some(menu.clone());
                        lsp_button.popover_menu_handle.refresh_menu(
                            window,
                            cx,
                            Rc::new(move |_, _| Some(menu.clone())),
                        );
                        cx.notify();
                    })
                    .ok();
            });
        }
    }
}

impl StatusItemView for LspButton {
    fn set_active_pane_item(
        &mut self,
        active_pane_item: Option<&dyn workspace::ItemHandle>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if ProjectSettings::get_global(cx).global_lsp_settings.button {
            if let Some(editor) = active_pane_item.and_then(|item| item.downcast::<Editor>()) {
                if Some(&editor)
                    != self
                        .server_state
                        .read(cx)
                        .active_editor
                        .as_ref()
                        .and_then(|active_editor| active_editor.editor.upgrade())
                        .as_ref()
                {
                    let editor_buffers = HashSet::from_iter(
                        editor
                            .read(cx)
                            .buffer()
                            .read(cx)
                            .snapshot(cx)
                            .excerpts()
                            .map(|excerpt| excerpt.context.start.buffer_id),
                    );
                    let _editor_subscription = cx.subscribe_in(
                        &editor,
                        window,
                        |lsp_button, _, e: &EditorEvent, window, cx| match e {
                            EditorEvent::BufferRangesUpdated { buffer, .. } => {
                                let updated = lsp_button.server_state.update(cx, |state, cx| {
                                    if let Some(active_editor) = state.active_editor.as_mut() {
                                        let buffer_id = buffer.read(cx).remote_id();
                                        active_editor.editor_buffers.insert(buffer_id)
                                    } else {
                                        false
                                    }
                                });
                                if updated {
                                    lsp_button.refresh_lsp_menu(false, window, cx);
                                }
                            }
                            EditorEvent::BuffersRemoved { removed_buffer_ids } => {
                                let removed = lsp_button.server_state.update(cx, |state, _| {
                                    let mut removed = false;
                                    if let Some(active_editor) = state.active_editor.as_mut() {
                                        for id in removed_buffer_ids {
                                            active_editor.editor_buffers.retain(|buffer_id| {
                                                let retain = buffer_id != id;
                                                removed |= !retain;
                                                retain
                                            });
                                        }
                                    }
                                    removed
                                });
                                if removed {
                                    lsp_button.refresh_lsp_menu(false, window, cx);
                                }
                            }
                            _ => {}
                        },
                    );
                    self.server_state.update(cx, |state, _| {
                        state.active_editor = Some(ActiveEditor {
                            editor: editor.downgrade(),
                            _editor_subscription,
                            editor_buffers,
                        });
                    });
                    self.refresh_lsp_menu(true, window, cx);
                }
            } else if self.server_state.read(cx).active_editor.is_some() {
                self.server_state.update(cx, |state, _| {
                    state.active_editor = None;
                });
                self.refresh_lsp_menu(false, window, cx);
            }
        } else if self.server_state.read(cx).active_editor.is_some() {
            self.server_state.update(cx, |state, _| {
                state.active_editor = None;
            });
            self.refresh_lsp_menu(false, window, cx);
        }
    }

    fn hide_setting(&self, _: &App) -> Option<workspace::HideStatusItem> {
        Some(workspace::HideStatusItem::new(|settings| {
            settings.global_lsp_settings.get_or_insert_default().button = Some(false);
        }))
    }
}

impl Render for LspButton {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl ui::IntoElement {
        let is_restricted = self
            .server_state
            .read(cx)
            .workspace
            .upgrade()
            .map(|workspace| {
                let worktree_store = workspace.read(cx).project().read(cx).worktree_store();
                TrustedWorktrees::has_restricted_worktrees(&worktree_store, cx)
            })
            .unwrap_or(false);

        if !is_restricted
            && (self.server_state.read(cx).language_servers.is_empty() || self.lsp_menu.is_none())
        {
            return div().hidden();
        }

        let state = self.server_state.read(cx);
        let is_via_ssh = state
            .workspace
            .upgrade()
            .map(|workspace| workspace.read(cx).project().read(cx).is_via_remote_server())
            .unwrap_or(false);

        let mut has_errors = false;
        let mut has_warnings = false;
        let mut has_other_notifications = false;
        for binary_status in state.language_servers.binary_statuses.values() {
            has_errors |= matches!(binary_status.status, BinaryStatus::Failed { .. });
            has_other_notifications |= binary_status.message.is_some();
        }

        for server in state.language_servers.health_statuses.values() {
            if let Some((message, health)) = &server.health {
                has_other_notifications |= message.is_some();
                match health {
                    ServerHealth::Ok => {}
                    ServerHealth::Warning => has_warnings = true,
                    ServerHealth::Error => has_errors = true,
                }
            }
        }

        let (indicator, description) = if is_restricted {
            (
                Some(Indicator::dot().color(Color::Warning)),
                "Restricted Mode",
            )
        } else if has_errors {
            (
                Some(Indicator::dot().color(Color::Error)),
                "Server with errors",
            )
        } else if has_warnings {
            (
                Some(Indicator::dot().color(Color::Warning)),
                "Server with warnings",
            )
        } else if has_other_notifications {
            (
                Some(Indicator::dot().color(Color::Modified)),
                "Server with notifications",
            )
        } else {
            (None, "All Servers Operational")
        };

        // SOLO: the servers' memory (with their child processes) next to the
        // button, and per server in the tooltip.
        let mut total_memory = 0;
        let mut server_lines = Vec::new();
        if let Some(lsp_store) = state.lsp_store.upgrade() {
            for (_, status) in lsp_store.read(cx).language_server_statuses() {
                let Some(usage) = status
                    .process_id
                    .and_then(|pid| state.server_usage.get(&pid).copied())
                else {
                    continue;
                };
                total_memory += usage.memory;
                server_lines.push(describe_server(
                    &status.name.0,
                    &server_activity(status.pending_work.values()),
                    Some(usage),
                ));
            }
        }
        server_lines.sort();
        let description: SharedString = if server_lines.is_empty() {
            description.into()
        } else {
            format!("{description}\n{}", server_lines.join("\n")).into()
        };
        let memory_label = (total_memory > 0).then(|| format_memory(total_memory));

        let lsp_button = cx.weak_entity();

        div()
            .child(
                PopoverMenu::new("lsp-tool")
                    .on_open(Rc::new(move |_window, cx| {
                        let copilot_enabled =
                            all_language_settings(None, cx).edit_predictions.provider
                                == EditPredictionProvider::Copilot;
                        telemetry::event!(
                            "Toolbar Menu Opened",
                            name = "Language Servers",
                            copilot_enabled,
                            is_via_ssh,
                        );
                    }))
                    .menu(move |_, cx| {
                        lsp_button
                            .read_with(cx, |lsp_button, _| lsp_button.lsp_menu.clone())
                            .ok()
                            .flatten()
                    })
                    .anchor(Anchor::BottomLeft)
                    .with_handle(self.popover_menu_handle.clone())
                    .trigger_with_tooltip(
                        IconButton::new("zed-lsp-tool-button", IconName::BoltOutlined)
                            .when_some(indicator, IconButton::indicator)
                            .icon_size(IconSize::Small)
                            .tab_index(0isize)
                            .aria_label("Language Servers")
                            .when(is_restricted, |s| s.icon_color(Color::Warning))
                            .indicator_border_color(Some(
                                cx.theme().colors().status_bar_background,
                            )),
                        move |_window, cx| {
                            Tooltip::with_meta(
                                "Language Servers",
                                Some(&ToggleMenu),
                                description.clone(),
                                cx,
                            )
                        },
                    ),
            )
            .when_some(memory_label, |this, memory_label| {
                this.flex().items_center().gap_0p5().child(
                    Label::new(memory_label)
                        .size(LabelSize::XSmall)
                        .color(Color::Muted),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server_id(n: usize) -> LanguageServerId {
        LanguageServerId(n)
    }

    fn server_name(s: &str) -> LanguageServerName {
        LanguageServerName(s.into())
    }

    fn health_status(name: &str) -> LanguageServerHealthStatus {
        LanguageServerHealthStatus {
            name: server_name(name),
            health: Some((None, ServerHealth::Ok)),
        }
    }

    fn servers_for_path(servers: &[(LanguageServerId, &str)]) -> ServersForPath {
        ServersForPath {
            servers: servers
                .iter()
                .map(|(id, name)| (*id, Some(server_name(name))))
                .collect(),
            worktree: None,
        }
    }

    /// `remove_server` evicts the id from `health_statuses` so a restarted
    /// server's new id renders without inheriting the old one's stale entry.
    /// This is the regression test for #53627.
    #[test]
    fn remove_server_drops_health_entry_for_id() {
        let mut state = LanguageServers::default();
        state
            .health_statuses
            .insert(server_id(1), health_status("rust-analyzer"));
        state
            .health_statuses
            .insert(server_id(2), health_status("typescript-language-server"));

        state.remove_server(server_id(1));

        assert!(!state.health_statuses.contains_key(&server_id(1)));
        assert!(state.health_statuses.contains_key(&server_id(2)));
    }

    /// `remove_server` evicts the id from each per-buffer entry; entries that
    /// become empty are dropped so the map does not grow unbounded across
    /// many buffer opens/closes.
    #[test]
    fn remove_server_evicts_id_from_per_buffer_entries_and_drops_empty_entries() {
        let mut state = LanguageServers::default();
        let buffer_a = PathBuf::from("/project/a.rs");
        let buffer_b = PathBuf::from("/project/b.rs");

        state.servers_per_buffer_abs_path.insert(
            buffer_a.clone(),
            servers_for_path(&[(server_id(1), "rust-analyzer")]),
        );
        state.servers_per_buffer_abs_path.insert(
            buffer_b.clone(),
            servers_for_path(&[(server_id(1), "rust-analyzer"), (server_id(2), "typos-lsp")]),
        );

        state.remove_server(server_id(1));

        assert!(
            !state.servers_per_buffer_abs_path.contains_key(&buffer_a),
            "buffer_a's entry held only the removed server, so the entry itself should be dropped",
        );
        let buffer_b_entry = state
            .servers_per_buffer_abs_path
            .get(&buffer_b)
            .expect("buffer_b's entry has another server, so it must be retained");
        assert!(!buffer_b_entry.servers.contains_key(&server_id(1)));
        assert!(buffer_b_entry.servers.contains_key(&server_id(2)));
    }

    /// `binary_statuses` is keyed by name and intentionally shared across
    /// restart cycles to drive the "Downloading… → Starting…" UX. Removing a
    /// single server's id must not touch it.
    #[test]
    fn remove_server_does_not_touch_binary_statuses() {
        let mut state = LanguageServers::default();
        state.binary_statuses.insert(
            server_name("rust-analyzer"),
            LanguageServerBinaryStatus {
                status: BinaryStatus::Starting,
                message: None,
            },
        );

        state.remove_server(server_id(1));

        assert!(
            state
                .binary_statuses
                .contains_key(&server_name("rust-analyzer")),
            "binary_statuses is name-keyed and shared across restart cycles",
        );
    }

    /// Simulates the full restart event sequence: remove old id, register
    /// new id with same name, write health for the new id. After restart
    /// only the new id should be visible — no leftover entry from the old
    /// incarnation.
    #[test]
    fn restart_sequence_leaves_only_new_server_id() {
        let mut state = LanguageServers::default();
        let buffer = PathBuf::from("/project/main.rs");
        let name = "rust-analyzer";

        // Pre-restart: server v1 is registered for the buffer with health.
        state
            .servers_per_buffer_abs_path
            .insert(buffer.clone(), servers_for_path(&[(server_id(1), name)]));
        state
            .health_statuses
            .insert(server_id(1), health_status(name));

        // Restart: old id is removed.
        state.remove_server(server_id(1));

        // New id registers for the same buffer.
        let entry = state
            .servers_per_buffer_abs_path
            .entry(buffer.clone())
            .or_insert_with(|| ServersForPath {
                servers: HashMap::default(),
                worktree: None,
            });
        entry.servers.insert(server_id(2), Some(server_name(name)));

        // Health update for the new id arrives.
        state
            .health_statuses
            .insert(server_id(2), health_status(name));

        let entry = state
            .servers_per_buffer_abs_path
            .get(&buffer)
            .expect("buffer must still be tracked");
        assert_eq!(
            entry.servers.keys().copied().collect::<Vec<_>>(),
            vec![server_id(2)],
            "exactly one server for this buffer — the new incarnation",
        );
        assert!(
            !state.health_statuses.contains_key(&server_id(1)),
            "the dead server's health entry must not linger",
        );
        assert!(
            state.health_statuses.contains_key(&server_id(2)),
            "the new server's health entry is present",
        );
    }

    #[test]
    fn tooltip_for_server_binary_handles_runtime_and_standalone_servers() {
        let node_server = lsp::LanguageServerBinary {
            path: "/usr/bin/node".into(),
            arguments: vec![
                "/zed/languages/basedpyright/langserver.index.js".into(),
                "--stdio".into(),
            ],
            env: None,
        };
        assert_eq!(
            tooltip_for_server_binary(&node_server, PathStyle::Unix),
            "/zed/languages/basedpyright/langserver.index.js (node)"
        );

        let node_server_windows = lsp::LanguageServerBinary {
                path: "C:\\Program Files\\nodejs\\node.exe".into(),
                arguments: vec![
                    "C:\\Users\\Zed\\languages\\basedpyright\\node_modules/basedpyright/langserver.index.js".into(),
                    "--stdio".into(),
                ],
                env: None
        };
        assert_eq!(
            tooltip_for_server_binary(&node_server_windows, PathStyle::Windows),
            "C:\\Users\\Zed\\languages\\basedpyright\\node_modules\\basedpyright\\langserver.index.js (node)"
        );

        let python_server = lsp::LanguageServerBinary {
            path: "/usr/bin/python3".into(),
            arguments: vec!["/zed/languages/pylsp/pylsp".into(), "--stdio".into()],
            env: None,
        };
        assert_eq!(
            tooltip_for_server_binary(&python_server, PathStyle::Unix),
            "/zed/languages/pylsp/pylsp (python)"
        );

        let standalone_server = lsp::LanguageServerBinary {
            path: "/usr/bin/ty".into(),
            arguments: vec!["server".into()],
            env: None,
        };
        assert_eq!(
            tooltip_for_server_binary(&standalone_server, PathStyle::Unix),
            "/usr/bin/ty"
        );

        let flagged_node_server = lsp::LanguageServerBinary {
            path: "/usr/bin/node".into(),
            arguments: vec![
                "--max-old-space-size=8192".into(),
                "/zed/languages/eslint/server.js".into(),
                "--stdio".into(),
            ],
            env: None,
        };
        assert_eq!(
            tooltip_for_server_binary(&flagged_node_server, PathStyle::Unix),
            "/zed/languages/eslint/server.js (node)"
        );
    }
}
