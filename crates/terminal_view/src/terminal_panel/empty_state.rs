//! SOLO: what the terminal column of the terminal-first layout shows once its
//! last terminal closes. The column stays open (the editor area beside it is
//! often empty too, which left a blank window), with a button for a new
//! terminal, one for each of the first few agent CLIs installed, a way to
//! install more and, when there's room, the file finder.

use gpui::{AnyElement, App, Context, Pixels, Window, canvas, px};
use settings::{BottomDockLayout, Settings as _};
use task::RevealStrategy;
use ui::{ButtonLike, KeyBinding, TabBar, TintColor, Tooltip, prelude::*};
use util::i18n::{is_chinese, tr};
use workspace::{
    NewTerminal, ToggleFileFinder, WorkspaceSettings,
    dock::{DockPosition, Panel as _},
};

use super::TerminalPanel;
use crate::{
    agent_install::{
        AgentInventory, EMPTY_STATE_LIMIT, agent_button, is_local, listed_agents,
        new_terminal_label, show_install_window,
    },
    default_working_directory,
};

/// Narrower than this, the column shows the compact empty state.
const COMPACT_BELOW: Pixels = px(360.);

#[derive(Default)]
pub(super) struct EmptyState {
    /// The column's width when last drawn.
    width: Option<Pixels>,
}

impl TerminalPanel {
    /// Whether closing the last terminal keeps the panel open: it's the
    /// terminal-first layout's main column.
    pub(super) fn keeps_column_when_empty(&self, window: &Window, cx: &App) -> bool {
        WorkspaceSettings::get_global(cx).bottom_dock_layout == BottomDockLayout::TerminalFirst
            && self.position(window, cx) == DockPosition::Bottom
    }

    /// Whether the panel shows the empty state: it has no terminals and none
    /// on the way, and stays open for want of them.
    pub(super) fn shows_empty_state(&self, window: &Window, cx: &App) -> bool {
        !self.restoring
            && self.pending_terminals_to_add == 0
            && self
                .center
                .panes()
                .iter()
                .all(|pane| pane.read(cx).items_len() == 0)
            && self.keeps_column_when_empty(window, cx)
    }

    /// Opens a terminal in the panel, as the empty state's button does.
    pub(super) fn new_terminal_from_empty_state(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Ok(working_directory) = self
            .workspace
            .read_with(cx, |workspace, cx| default_working_directory(workspace, cx))
        else {
            return;
        };
        self.add_terminal_shell(false, working_directory, RevealStrategy::Always, window, cx)
            .detach_and_log_err(cx);
    }

    pub(super) fn render_empty_state(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let compact = self
            .empty_state
            .width
            .is_some_and(|width| width < COMPACT_BELOW);
        let focus_handle = self.focus_handle.clone();
        let workspace = self.workspace.clone();
        let is_local = is_local(&workspace, cx);
        let scanned = AgentInventory::global(cx).read(cx).scanned();
        let agents: Vec<_> = listed_agents(&workspace, cx)
            .into_iter()
            .take(EMPTY_STATE_LIMIT)
            .collect();
        let no_agents_found = is_local && scanned && agents.is_empty();

        let new_terminal = Button::new(
            "terminal-empty-new-terminal",
            tr("New Terminal", "新建终端"),
        )
        .style(ButtonStyle::Tinted(TintColor::Accent))
        .size(ButtonSize::Large)
        .key_binding(
            KeyBinding::for_action_in(&NewTerminal::default(), &focus_handle, cx)
                .size(rems_from_px(12_f32)),
        )
        .on_click(
            cx.listener(|panel, _, window, cx| panel.new_terminal_from_empty_state(window, cx)),
        );

        let agent_buttons = agents.into_iter().map(|agent| {
            let label: SharedString = if compact {
                agent.spec.name.into()
            } else {
                new_terminal_label(agent.spec.name).into()
            };
            let workspace = workspace.clone();
            let command = agent.spec.command;
            agent_button(
                format!("terminal-empty-agent-{command}").into(),
                &agent,
                label,
                cx.theme().colors().terminal_background,
                cx,
            )
            .on_click(move |_, window, cx| {
                workspace
                    .update(cx, |workspace, cx| {
                        crate::agent_context::start_in_new_terminal(workspace, command, window, cx)
                    })
                    .ok();
            })
        });
        let no_agents = no_agents_found.then(|| {
            div()
                .debug_selector(|| "terminal-empty-no-agents".into())
                .child(
                    Label::new(tr("No agents found", "没找到 agent"))
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
        });
        let install_agents = is_local.then(|| {
            let workspace = workspace.clone();
            let button = ButtonLike::new("terminal-empty-install-agents")
                .style(ButtonStyle::Outlined)
                .size(ButtonSize::Medium)
                .full_width()
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            div()
                                .flex_none()
                                .size(rems_from_px(16_f32))
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(
                                    Icon::new(IconName::Download)
                                        .size(IconSize::Small)
                                        .color(Color::Muted),
                                ),
                        )
                        .child(
                            Label::new(tr("Install Agents…", "安装 agent…")).size(LabelSize::Small),
                        ),
                )
                .on_click(move |_, window, cx| show_install_window(&workspace, window, cx));
            div()
                .w_full()
                .debug_selector(|| "terminal-empty-install-agents".into())
                .child(button)
        });

        let body = if compact {
            v_flex()
                .flex_1()
                .min_h_0()
                .justify_center()
                .gap_2p5()
                .px_4()
                .child(
                    h_flex()
                        .justify_center()
                        .child(Label::new(tr("No terminals yet", "还没有终端"))),
                )
                .child(new_terminal.full_width())
                .children(agent_buttons)
                .children(no_agents)
                .children(install_agents)
                .into_any_element()
        } else {
            let directory = self
                .workspace
                .read_with(cx, |workspace, cx| default_working_directory(workspace, cx))
                .ok()
                .flatten();
            let find_file =
                ButtonLike::new("terminal-empty-find-file")
                    .style(ButtonStyle::Outlined)
                    .size(ButtonSize::Medium)
                    .full_width()
                    .child(
                        h_flex()
                            .w_full()
                            .gap_2()
                            .child(
                                Icon::new(IconName::MagnifyingGlass)
                                    .size(IconSize::Small)
                                    .color(Color::Muted),
                            )
                            .child(div().flex_1().child(
                                Label::new(tr("Find File", "查找文件")).size(LabelSize::Small),
                            ))
                            .child(
                                KeyBinding::for_action_in(
                                    &ToggleFileFinder::default(),
                                    &focus_handle,
                                    cx,
                                )
                                .size(rems_from_px(11_f32)),
                            ),
                    )
                    .on_click({
                        let focus_handle = focus_handle.clone();
                        move |_, window, cx| {
                            focus_handle.dispatch_action(&ToggleFileFinder::default(), window, cx)
                        }
                    });
            v_flex()
                .flex_1()
                .min_h_0()
                .items_center()
                .justify_center()
                .gap_3p5()
                .px_6()
                .child(
                    Icon::new(IconName::Terminal)
                        .size(IconSize::Custom(rems_from_px(30_f32)))
                        .color(Color::Muted),
                )
                .child(
                    v_flex()
                        .items_center()
                        .gap_1()
                        .max_w_full()
                        .child(Label::new(tr(
                            "No terminals in this tab yet",
                            "这个标签里还没有终端",
                        )))
                        .when_some(directory, |this, directory| {
                            let (before, after) = if is_chinese() {
                                ("新终端在", "打开")
                            } else {
                                ("New terminals open in", "")
                            };
                            this.child(
                                h_flex()
                                    .max_w_full()
                                    .gap_1()
                                    .child(
                                        Label::new(before)
                                            .size(LabelSize::Small)
                                            .color(Color::Muted)
                                            .untranslated(),
                                    )
                                    .child(
                                        div().min_w_0().child(
                                            Label::new(directory.to_string_lossy().into_owned())
                                                .size(LabelSize::Small)
                                                .color(Color::Muted)
                                                .buffer_font(cx)
                                                .truncate_start()
                                                .untranslated(),
                                        ),
                                    )
                                    .when(!after.is_empty(), |this| {
                                        this.child(
                                            Label::new(after)
                                                .size(LabelSize::Small)
                                                .color(Color::Muted)
                                                .untranslated(),
                                        )
                                    }),
                            )
                        }),
                )
                .child(new_terminal)
                .child(
                    v_flex()
                        .min_w(px(240.))
                        .gap_1()
                        .children(agent_buttons)
                        .children(no_agents.map(|label| label.py_1()))
                        .children(install_agents)
                        .child(find_file),
                )
                .into_any_element()
        };

        let panel = cx.entity().downgrade();
        v_flex()
            .id("terminal-empty-state")
            .debug_selector(|| "terminal-empty-state".into())
            .absolute()
            .inset_0()
            .bg(cx.theme().colors().terminal_background)
            .child(
                TabBar::new("terminal-empty-tab-bar").end_child(
                    IconButton::new("terminal-empty-plus", IconName::Plus)
                        .icon_size(IconSize::Small)
                        .tooltip(move |_window, cx| {
                            Tooltip::for_action_in(
                                tr("New Terminal", "新建终端"),
                                &NewTerminal::default(),
                                &focus_handle,
                                cx,
                            )
                        })
                        .on_click(cx.listener(|panel, _, window, cx| {
                            panel.new_terminal_from_empty_state(window, cx)
                        })),
                ),
            )
            .child(body)
            .child(
                canvas(
                    move |bounds, _, cx| {
                        panel
                            .update(cx, |panel, cx| {
                                if panel.empty_state.width != Some(bounds.size.width) {
                                    panel.empty_state.width = Some(bounds.size.width);
                                    cx.notify();
                                }
                            })
                            .ok();
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            )
            .into_any_element()
    }
}
