//! SOLO: the first launch's introduction to running AI agents in Solo's
//! terminal: which agent CLIs are installed, what Solo adds to them, and what
//! it leaves alone.

use gpui::{App, IntoElement};
use ui::{Tooltip, prelude::*};
use util::i18n::{is_chinese, tr};

/// Agent CLIs Solo knows, with the command that runs each.
pub(crate) const TERMINAL_AGENTS: &[(&str, &str)] = &[
    ("Claude Code", "claude"),
    ("Codex", "codex"),
    ("Gemini CLI", "gemini"),
    ("GitHub Copilot CLI", "copilot"),
    ("OpenCode", "opencode"),
    ("Cursor Agent", "cursor-agent"),
    ("Qwen Code", "qwen"),
    ("Amp", "amp"),
    ("Aider", "aider"),
];

const GUIDE_URL: &str = "https://github.com/lucas9801/solo/blob/main/docs/guide/en.md";
const GUIDE_URL_ZH: &str = "https://github.com/lucas9801/solo/blob/main/docs/guide/zh-CN.md";

/// Which of the agents are installed: their command is on the `PATH`.
pub(crate) fn detect_installed_agents() -> Vec<(&'static str, bool)> {
    TERMINAL_AGENTS
        .iter()
        .map(|(name, command)| (*name, which::which(command).is_ok()))
        .collect()
}

/// The agents found first, then the rest, keeping their order otherwise.
pub(crate) fn installed_first(agents: &[(&'static str, bool)]) -> Vec<(&'static str, bool)> {
    let mut agents = agents.to_vec();
    agents.sort_by_key(|(_, installed)| !installed);
    agents
}

fn render_agent(name: &'static str, installed: bool) -> impl IntoElement {
    h_flex()
        .gap_1()
        .px_2()
        .py_0p5()
        .rounded_md()
        .border_1()
        .border_color(gpui::transparent_black())
        .child(
            Icon::new(if installed {
                IconName::Check
            } else {
                IconName::Dash
            })
            .size(IconSize::XSmall)
            .color(if installed {
                Color::Success
            } else {
                Color::Disabled
            }),
        )
        .child(Label::new(name).size(LabelSize::Small).color(if installed {
            Color::Default
        } else {
            Color::Muted
        }))
}

/// The section on agents in the terminal; `detected` is `None` while the
/// installed agents are still being looked for.
pub(crate) fn render_terminal_agents_section(
    detected: Option<&[(&'static str, bool)]>,
    cx: &mut App,
) -> impl IntoElement {
    let found = detected.map(|agents| agents.iter().filter(|(_, installed)| *installed).count());
    let status: SharedString = match found {
        None => tr("Looking for installed agents…", "正在查找已安装的 agent…").into(),
        Some(0) => tr(
            "No agent CLI found on the PATH. Install one, such as Claude Code or Codex, then run it in a terminal here.",
            "PATH 里没有找到 agent 命令行工具。装一个（比如 Claude Code 或 Codex），然后在这里的终端里运行。",
        )
        .into(),
        Some(count) if is_chinese() => format!("找到 {count} 个已安装的 agent：").into(),
        Some(1) => "Found 1 installed agent:".into(),
        Some(count) => format!("Found {count} installed agents:").into(),
    };
    let agents = detected.map(installed_first).unwrap_or_default();
    let border = cx.theme().colors().border_variant;

    v_flex()
        .gap_2()
        .child(Label::new(tr("Agents in the Terminal", "在终端里用 agent")))
        .child(
            Label::new(tr(
                "Run Claude Code, Codex or another agent CLI in Solo's terminal, as you would anywhere. Solo shows what each agent is doing, keeps every turn's changes for you to review, reject or roll back, and sends code, comments and errors to it.",
                "像在别处一样，在 Solo 的终端里运行 Claude Code、Codex 等 agent 命令行工具。Solo 会显示每个 agent 在做什么，把每一轮的改动留给你审阅、拒绝或回滚，还能把代码、批注和错误发给它。",
            ))
            .color(Color::Muted),
        )
        .child(
            v_flex()
                .gap_1()
                .p_3()
                .rounded_md()
                .border_1()
                .border_color(border)
                .child(Label::new(status).size(LabelSize::Small))
                .when(!agents.is_empty(), |this| {
                    this.child(
                        h_flex().flex_wrap().gap_1().children(
                            agents
                                .into_iter()
                                .map(|(name, installed)| render_agent(name, installed)),
                        ),
                    )
                }),
        )
        .child(
            h_flex()
                .gap_2()
                .items_start()
                .child(
                    Icon::new(IconName::Info)
                        .size(IconSize::Small)
                        .color(Color::Muted),
                )
                .child(
                    Label::new(tr(
                        "Solo adds its hooks only to the sessions started in its terminal, through their environment, and doesn't change your agents' own settings files. Agents run on your own CLIs and subscriptions.",
                        "Solo 只给在它的终端里启动的会话加 hooks（通过这些会话的环境变量），不改动 agent 自己的配置文件。agent 用的是你自己的命令行工具和订阅。",
                    ))
                    .size(LabelSize::Small)
                    .color(Color::Muted),
                ),
        )
        .child(
            h_flex().gap_2().child(
                Button::new("open-solo-guide", tr("Read the Guide", "阅读用户指南"))
                    .style(ButtonStyle::Outlined)
                    .end_icon(
                        Icon::new(IconName::ArrowUpRight)
                            .size(IconSize::XSmall)
                            .color(Color::Muted),
                    )
                    .tooltip(Tooltip::text(tr(
                        "Installing, the first agent, reviewing turns and troubleshooting",
                        "安装、第一个 agent、按轮审阅和故障排查",
                    )))
                    .on_click(|_, _, cx| {
                        cx.open_url(if is_chinese() { GUIDE_URL_ZH } else { GUIDE_URL })
                    }),
            ),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_installed_first() {
        let agents = [("A", false), ("B", true), ("C", false), ("D", true)];
        assert_eq!(
            installed_first(&agents),
            vec![("B", true), ("D", true), ("A", false), ("C", false)]
        );
    }

    #[test]
    fn test_detects_every_agent() {
        let detected = detect_installed_agents();
        assert_eq!(detected.len(), TERMINAL_AGENTS.len());
    }
}
