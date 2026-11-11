//! SOLO: what the terminal column of the terminal-first layout shows once its
//! last terminal closes. The column stays open (the editor area beside it is
//! often empty too, which left a blank window), with a button for a new
//! terminal, one for each agent CLI found on the `PATH` and, when there's room,
//! the file finder.

use gpui::{AnyElement, App, Context, Pixels, Task, Window, canvas, px};
use settings::{BottomDockLayout, Settings as _};
use task::RevealStrategy;
use ui::{ButtonLike, KeyBinding, TabBar, TintColor, Tooltip, prelude::*};
use util::i18n::{is_chinese, tr};
use workspace::{
    NewTerminal, ToggleFileFinder, WorkspaceSettings,
    dock::{DockPosition, Panel as _},
};

use super::TerminalPanel;
use crate::{agent_tab::render_agent_mark, default_working_directory};

/// Agent CLIs the empty state offers to start: their name and command.
const AGENTS: &[(&str, &str)] = &[("Claude Code", "claude"), ("Codex", "codex")];

/// Narrower than this, the column shows the compact empty state.
const COMPACT_BELOW: Pixels = px(360.);

/// An agent CLI the empty state can start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Agent {
    pub name: &'static str,
    pub command: &'static str,
}

/// The agents whose command `is_on_path` finds, in [`AGENTS`]' order.
pub(super) fn agents_on_path(is_on_path: impl Fn(&str) -> bool) -> Vec<Agent> {
    AGENTS
        .iter()
        .filter(|(_, command)| is_on_path(command))
        .map(|&(name, command)| Agent { name, command })
        .collect()
}

#[derive(Default)]
pub(super) struct EmptyState {
    /// The agents found on the `PATH`; `None` until the first look finishes.
    pub(super) agents: Option<Vec<Agent>>,
    looking: Option<Task<()>>,
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

    /// Looks for the agents on the `PATH` again, off the main thread. The
    /// previous result stays up meanwhile. A remote project's agents would be
    /// on the remote's `PATH`, so none are offered there.
    pub(super) fn find_agents(&mut self, cx: &mut Context<Self>) {
        let is_local = self
            .workspace
            .upgrade()
            .is_some_and(|workspace| workspace.read(cx).project().read(cx).is_local());
        if !is_local {
            self.empty_state.agents = Some(Vec::new());
            return;
        }
        self.empty_state.looking = Some(cx.spawn(async move |panel, cx| {
            let agents = cx
                .background_spawn(
                    async move { agents_on_path(|command| which::which(command).is_ok()) },
                )
                .await;
            panel
                .update(cx, |panel, cx| {
                    panel.empty_state.agents = Some(agents);
                    panel.empty_state.looking = None;
                    cx.notify();
                })
                .ok();
        }));
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
        if self.empty_state.agents.is_none() && self.empty_state.looking.is_none() {
            self.find_agents(cx);
        }
        let compact = self
            .empty_state
            .width
            .is_some_and(|width| width < COMPACT_BELOW);
        let focus_handle = self.focus_handle.clone();
        let agents = self.empty_state.agents.clone().unwrap_or_default();
        let workspace = self.workspace.clone();

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
                agent.name.into()
            } else if is_chinese() {
                format!("新建 {} 终端", agent.name).into()
            } else {
                format!("New {} Terminal", agent.name).into()
            };
            let workspace = workspace.clone();
            ButtonLike::new(SharedString::from(format!(
                "terminal-empty-agent-{}",
                agent.command
            )))
            .style(ButtonStyle::Outlined)
            .size(ButtonSize::Medium)
            .full_width()
            .child(
                h_flex()
                    .gap_2()
                    .child(render_agent_mark(agent.name, cx))
                    .child(Label::new(label).size(LabelSize::Small).untranslated()),
            )
            .on_click(move |_, window, cx| {
                workspace
                    .update(cx, |workspace, cx| {
                        crate::agent_context::start_in_new_terminal(
                            workspace,
                            agent.command,
                            window,
                            cx,
                        )
                    })
                    .ok();
            })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agents_on_path() {
        assert_eq!(agents_on_path(|_| false), Vec::new());
        assert_eq!(
            agents_on_path(|command| command == "codex"),
            vec![Agent {
                name: "Codex",
                command: "codex"
            }]
        );
        assert_eq!(
            agents_on_path(|_| true)
                .iter()
                .map(|agent| agent.name)
                .collect::<Vec<_>>(),
            vec!["Claude Code", "Codex"]
        );
    }
}
