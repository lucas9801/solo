//! SOLO: the dialog offering to split a workspace holding unrelated projects
//! into a tab each (see `workspace_grouping`): which folders go, and where
//! the terminals in none of them go.

use std::path::PathBuf;

use gpui::{
    App, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, WeakEntity,
};
use ui::{ContextMenu, DropdownMenu, DropdownStyle, prelude::*};
use util::i18n::tr;
use workspace::{ModalView, Workspace};

use crate::{
    TerminalView, agent_tab,
    workspace_grouping::{Destination, Root, folder_name, keep_together, split_workspace_now},
};

/// A terminal whose directory is in none of the workspace's folders.
#[derive(Clone)]
pub(crate) struct LooseTerminal {
    pub(crate) view: WeakEntity<TerminalView>,
    /// Its session's title, or its shell's.
    pub(crate) title: String,
    /// The agent it runs, and whether it's still running.
    pub(crate) agent: Option<(String, bool)>,
    pub(crate) directory: PathBuf,
    /// Where it goes by default: the tab that has its project, or a new one.
    pub(crate) own: Destination,
    /// Its project's name.
    pub(crate) name: String,
}

impl LooseTerminal {
    fn own_label(&self) -> String {
        if self.own.key.is_some() {
            if util::i18n::is_chinese() {
                format!("移到 {} 的标签", self.own.name())
            } else {
                format!("Move to the {} Tab", self.own.name())
            }
        } else if util::i18n::is_chinese() {
            format!("新建 {} 标签", self.name)
        } else {
            format!("New {} Tab", self.name)
        }
    }
}

/// Where a loose terminal goes on splitting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Choice {
    /// Its [`LooseTerminal::own`] tab.
    Own,
    Stay,
    /// The tab of the split folder at this index.
    SplitOff(usize),
}

pub(crate) struct SplitWorkspaceModal {
    workspace: WeakEntity<Workspace>,
    roots: Vec<Root>,
    unrelated: Vec<Root>,
    loose: Vec<LooseTerminal>,
    choices: Vec<Choice>,
    /// Remembers the workspace was kept together.
    kept_key: String,
    focus_handle: FocusHandle,
}

impl SplitWorkspaceModal {
    pub(crate) fn new(
        workspace: WeakEntity<Workspace>,
        roots: Vec<Root>,
        unrelated: Vec<Root>,
        loose: Vec<LooseTerminal>,
        kept_key: String,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            workspace,
            roots,
            unrelated,
            choices: vec![Choice::Own; loose.len()],
            loose,
            kept_key,
            focus_handle: cx.focus_handle(),
        }
    }

    fn tab_name(&self) -> String {
        self.roots
            .first()
            .map(|root| folder_name(&root.path))
            .unwrap_or_default()
    }

    fn choice_label(&self, terminal: &LooseTerminal, choice: Choice) -> String {
        match choice {
            Choice::Own => terminal.own_label(),
            Choice::Stay => {
                if util::i18n::is_chinese() {
                    format!("留在 {}", self.tab_name())
                } else {
                    format!("Keep in {}", self.tab_name())
                }
            }
            Choice::SplitOff(ix) => {
                let name = self
                    .unrelated
                    .get(ix)
                    .map(|root| folder_name(&root.path))
                    .unwrap_or_default();
                if util::i18n::is_chinese() {
                    format!("移到 {name}")
                } else {
                    format!("Move to {name}")
                }
            }
        }
    }

    fn split(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace.upgrade() else {
            return cx.emit(DismissEvent);
        };
        let moves: Vec<(Entity<TerminalView>, Destination, Choice)> = self
            .loose
            .iter()
            .zip(&self.choices)
            .filter_map(|(terminal, choice)| {
                Some((terminal.view.upgrade()?, terminal.own.clone(), *choice))
            })
            .collect();
        cx.emit(DismissEvent);
        split_workspace_now(&workspace, &self.unrelated, moves, window, cx);
    }

    fn keep_together(&mut self, cx: &mut Context<Self>) {
        keep_together(self.kept_key.clone(), cx);
        cx.emit(DismissEvent);
    }

    fn cancel(&mut self, _: &menu::Cancel, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }
}

impl EventEmitter<DismissEvent> for SplitWorkspaceModal {}

impl Focusable for SplitWorkspaceModal {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ModalView for SplitWorkspaceModal {}

impl Render for SplitWorkspaceModal {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let small = |text: String| Label::new(text).size(LabelSize::Small);
        let section = |text: String| {
            Label::new(text)
                .size(LabelSize::Small)
                .color(Color::Muted)
                .weight(FontWeight::BOLD)
        };

        let folders = v_flex()
            .gap_1()
            .child(section(tr("Folders", "文件夹").to_string()))
            .children(self.roots.iter().map(|root| {
                let splits = self.unrelated.contains(root);
                h_flex()
                    .gap_2()
                    .justify_between()
                    .child(
                        h_flex()
                            .min_w_0()
                            .gap_1p5()
                            .child(
                                Icon::new(IconName::Folder)
                                    .size(IconSize::Small)
                                    .color(Color::Muted),
                            )
                            .child(small(folder_name(&root.path)).untranslated().truncate()),
                    )
                    .child(
                        small(
                            if splits {
                                tr("New tab", "新标签")
                            } else {
                                tr("Stays in this tab", "留在这个标签")
                            }
                            .to_string(),
                        )
                        .color(if splits {
                            Color::Accent
                        } else {
                            Color::Muted
                        }),
                    )
            }));

        let loose = (!self.loose.is_empty()).then(|| {
            let heading = if util::i18n::is_chinese() {
                format!(
                    "这 {} 个终端不在上面任何文件夹里，选一下去哪",
                    self.loose.len()
                )
            } else if self.loose.len() == 1 {
                "This terminal is in none of the folders above; choose where it goes".to_string()
            } else {
                format!(
                    "These {} terminals are in none of the folders above; choose where they go",
                    self.loose.len()
                )
            };
            let this = cx.entity().downgrade();
            v_flex()
                .gap_1p5()
                .child(section(heading))
                .children(self.loose.iter().enumerate().map(|(ix, terminal)| {
                    let mut options = vec![Choice::Own, Choice::Stay];
                    options.extend((0..self.unrelated.len()).map(Choice::SplitOff));
                    let labels: Vec<(Choice, String)> = options
                        .into_iter()
                        .map(|choice| (choice, self.choice_label(terminal, choice)))
                        .collect();
                    let current = self.choice_label(terminal, self.choices[ix]);
                    let this = this.clone();
                    let menu = ContextMenu::build(window, cx, move |mut menu, _, _| {
                        for (choice, label) in &labels {
                            let this = this.clone();
                            let choice = *choice;
                            menu = menu.entry(label.clone(), None, move |_, cx| {
                                this.update(cx, |modal, cx| {
                                    modal.choices[ix] = choice;
                                    cx.notify();
                                })
                                .ok();
                            });
                        }
                        menu
                    });
                    let detail = match &terminal.agent {
                        Some((agent, true)) => {
                            if util::i18n::is_chinese() {
                                format!("{agent} 正在运行 · {}", terminal.directory.display())
                            } else {
                                format!("{agent} running · {}", terminal.directory.display())
                            }
                        }
                        _ => terminal.directory.display().to_string(),
                    };
                    h_flex()
                        .gap_2()
                        .justify_between()
                        .child(
                            h_flex()
                                .min_w_0()
                                .gap_2()
                                .child(match &terminal.agent {
                                    Some((agent, _)) => agent_tab::render_agent_mark(agent, cx),
                                    None => Icon::new(IconName::Terminal)
                                        .size(IconSize::Small)
                                        .color(Color::Muted)
                                        .into_any_element(),
                                })
                                .child(
                                    v_flex()
                                        .min_w_0()
                                        .child(
                                            small(terminal.title.clone()).untranslated().truncate(),
                                        )
                                        .child(
                                            Label::new(detail)
                                                .untranslated()
                                                .size(LabelSize::XSmall)
                                                .color(Color::Muted)
                                                .truncate(),
                                        ),
                                ),
                        )
                        .child(
                            div().flex_none().child(
                                DropdownMenu::new(("loose-terminal", ix), current, menu)
                                    .style(DropdownStyle::Outlined),
                            ),
                        )
                }))
        });

        v_flex()
            .key_context("SplitWorkspaceModal")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::cancel))
            .w(rems(36.))
            .elevation_3(cx)
            .p_4()
            .gap_4()
            .child(
                v_flex()
                    .gap_1()
                    .child(Label::new(tr("Split Workspace", "拆分工作区")).weight(FontWeight::BOLD))
                    .child(
                        small(
                            tr(
                                "This workspace holds unrelated projects: split them into a tab each. Terminals go with their directories, and what runs in them carries on.",
                                "这个工作区里有互不相关的项目，拆成各自的标签。终端跟着自己的目录走，正在运行的程序不受影响。",
                            )
                            .to_string(),
                        )
                        .color(Color::Muted),
                    ),
            )
            .child(folders)
            .children(loose)
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("keep-workspace-together", tr("Keep Together", "保持现状"))
                            .on_click(cx.listener(|this, _, _, cx| this.keep_together(cx))),
                    )
                    .child(
                        Button::new("split-workspace", tr("Split", "拆分"))
                            .style(ButtonStyle::Filled)
                            .on_click(cx.listener(|this, _, window, cx| this.split(window, cx))),
                    ),
            )
    }
}

#[cfg(test)]
impl SplitWorkspaceModal {
    pub(crate) fn loose_labels(&self) -> Vec<Vec<String>> {
        self.loose
            .iter()
            .map(|terminal| {
                let mut options = vec![Choice::Own, Choice::Stay];
                options.extend((0..self.unrelated.len()).map(Choice::SplitOff));
                options
                    .into_iter()
                    .map(|choice| self.choice_label(terminal, choice))
                    .collect()
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    /// Workspace 2 on 2026-11-09: player and Dragon_Art split off
    /// godot_engine, two terminals in DragonNest.
    #[gpui::test]
    fn test_loose_terminal_choices(cx: &mut TestAppContext) {
        let root = |path: &str| Root {
            path: PathBuf::from(path),
            repository: None,
        };
        let roots = vec![
            root("/work/Godot/godot_engine"),
            root("/work/player"),
            root("/work/Dragon_Art"),
        ];
        let unrelated = roots[1..].to_vec();
        let terminal = |title: &str, agent: Option<(String, bool)>| LooseTerminal {
            view: WeakEntity::new_invalid(),
            title: title.to_string(),
            agent,
            directory: PathBuf::from("/work/DragonNest"),
            own: Destination {
                key: None,
                paths: vec![PathBuf::from("/work/DragonNest")],
                workspace: None,
            },
            name: "DragonNest".to_string(),
        };
        let loose = vec![
            terminal("龙之谷游戏引擎", Some(("claude".to_string(), true))),
            terminal("powershell", None),
        ];
        let modal = cx.new(|cx| {
            SplitWorkspaceModal::new(
                WeakEntity::new_invalid(),
                roots,
                unrelated,
                loose,
                "key".to_string(),
                cx,
            )
        });
        modal.read_with(cx, |modal, _| {
            // Each goes to a new tab of its own unless chosen otherwise.
            assert_eq!(modal.choices, [Choice::Own, Choice::Own]);
            let expected: Vec<String> = vec![
                tr("New DragonNest Tab", "新建 DragonNest 标签").to_string(),
                tr("Keep in godot_engine", "留在 godot_engine").to_string(),
                tr("Move to player", "移到 player").to_string(),
                tr("Move to Dragon_Art", "移到 Dragon_Art").to_string(),
            ];
            assert_eq!(modal.loose_labels(), [expected.clone(), expected]);
        });
    }
}
