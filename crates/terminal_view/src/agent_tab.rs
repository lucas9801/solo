//! SOLO: how an agent's terminal tab shows the agent: its icon with its
//! status on it, how full its context is, and the card shown on hover.

use agent_sessions::{
    CONTEXT_ALMOST_OUT, CONTEXT_NEARLY_FULL, MatchedBy, SessionState, TokenUsage,
};
use gpui::{AnyElement, App, FontWeight, Hsla};
use ui::{CircularProgress, Divider, prelude::*};
use util::i18n::tr;
use workspace::agent_activity::{AgentStateIcon, legible_on};

use crate::{TerminalView, agent_status::AgentStatus};

impl TerminalView {
    /// The card shown on the terminal's tab; `None` before its process runs.
    pub(crate) fn tab_card(&self, cx: &App) -> Option<TabCard> {
        let terminal = self.terminal().read(cx);
        let pid = terminal.pid_getter()?.fallback_pid().as_u32();
        let agent = self.agent_status().map(|(name, status)| {
            let session = self.agent_session().map(|session| session.read(cx));
            AgentCard::new(
                name,
                Some(status),
                session.map(|session| (session.state(), session.location().matched_by)),
            )
        });
        Some(TabCard {
            title: self.title_with(0, false, agent.is_some(), cx),
            agent,
            location: self.title_details(cx),
            // SOLO: a terminal not in its tab's folders says so under where
            // it is.
            location_note: self.grouping.misplaced.as_ref().and_then(|misplaced| {
                Some((
                    misplaced.directory.display().to_string(),
                    misplaced.card_note()?,
                ))
            }),
            // See `Terminal::host_failed`.
            warning: terminal.host_failed().then(|| {
                tr(
                    "Ends when Solo quits: its background process couldn't be started",
                    "Solo 退出时它会结束：没能启动它的后台进程",
                )
            }),
            pid: Some(pid),
        })
    }
}

/// The color of a context `fraction` full: gray, yellow from 80%, red from 95%.
pub(crate) fn context_color(fraction: f64) -> Color {
    if fraction >= CONTEXT_ALMOST_OUT {
        Color::Error
    } else if fraction >= CONTEXT_NEARLY_FULL {
        Color::Warning
    } else {
        Color::Muted
    }
}

/// The context's color, legible on `background`.
pub(crate) fn context_hsla(fraction: f64, background: Hsla, cx: &App) -> Hsla {
    legible_on(context_color(fraction).color(cx), background)
}

/// A token count, briefly: 950, 12.3k, 4.5M.
pub(crate) fn format_tokens(tokens: u64) -> String {
    match tokens {
        0..1_000 => tokens.to_string(),
        1_000..1_000_000 => format!("{:.1}k", tokens as f64 / 1_000.),
        _ => format!("{:.1}M", tokens as f64 / 1_000_000.),
    }
}

/// A token count to three significant digits: 482, 12.3k, 542k, 1.1M, 211M.
pub(crate) fn format_tokens_short(tokens: u64) -> String {
    let (value, unit) = match tokens {
        0..1_000 => return tokens.to_string(),
        1_000..1_000_000 => (tokens as f64 / 1_000., "k"),
        _ => (tokens as f64 / 1_000_000., "M"),
    };
    let decimals = if value >= 100. {
        0
    } else if value >= 10. {
        1
    } else {
        2
    };
    let rounded = format!("{value:.decimals$}");
    if unit == "k"
        && rounded
            .parse::<f64>()
            .is_ok_and(|rounded| rounded >= 1_000.)
    {
        return format_tokens_short(1_000_000);
    }
    let rounded = if rounded.contains('.') {
        rounded.trim_end_matches('0').trim_end_matches('.')
    } else {
        &rounded
    };
    format!("{rounded}{unit}")
}

/// The model's name without what the agent already says: claude-opus-5-5 is
/// opus-5-5, and a date at the end goes.
pub(crate) fn short_model(model: &str) -> &str {
    let model = model.strip_prefix("claude-").unwrap_or(model);
    match model.rsplit_once('-') {
        Some((name, date))
            if !name.is_empty() && date.len() == 8 && date.bytes().all(|b| b.is_ascii_digit()) =>
        {
            name
        }
        _ => model,
    }
}

/// The agent's icon, Zed's for the agents it has one for.
fn agent_icon(name: &str) -> Option<IconName> {
    let name = name.to_lowercase();
    if name.starts_with("claude") {
        Some(IconName::AiClaude)
    } else if name.starts_with("codex") {
        Some(IconName::AiOpenAi)
    } else if name.starts_with("copilot") {
        Some(IconName::Copilot)
    } else {
        None
    }
}

/// The agent's icon, or its initial when it has none, in a small square.
pub(crate) fn render_agent_mark(name: &str, cx: &App) -> AnyElement {
    let mark = div()
        .flex_none()
        .size(rems_from_px(16_f32))
        .rounded(px(4.))
        .bg(cx.theme().colors().border)
        .flex()
        .items_center()
        .justify_center();
    match agent_icon(name) {
        Some(icon) => mark.child(Icon::new(icon).size(IconSize::XSmall).color(Color::Default)),
        None => {
            let initial = name
                .chars()
                .find(|c| c.is_alphanumeric())
                .map(|c| c.to_uppercase().to_string())
                .unwrap_or_else(|| "?".to_string());
            mark.child(
                Label::new(initial)
                    .untranslated()
                    .size(LabelSize::Custom(rems_from_px(10_f32)))
                    .weight(FontWeight::BOLD),
            )
        }
    }
    .into_any_element()
}

/// The agent's mark with its status on its bottom right corner, ringed in
/// `backdrop` (the tab's background) to stand off the mark.
pub(crate) fn render_agent_badge(
    name: &str,
    status: AgentStatus,
    phase: Option<f32>,
    backdrop: Hsla,
    cx: &App,
) -> AnyElement {
    let mut icon = AgentStateIcon::new(status)
        .size(IconSize::Custom(rems_from_px(11_f32)))
        .on(backdrop);
    if let Some(phase) = phase {
        icon = icon.phase(phase);
    }
    div()
        .debug_selector(|| "agent-tab-badge".into())
        .relative()
        .flex_none()
        // Room for the status, which sticks out.
        .mr_1()
        .child(render_agent_mark(name, cx))
        .child(
            div()
                .absolute()
                .right(px(-4.))
                .bottom(px(-4.))
                .p(px(1.))
                .rounded_full()
                .bg(backdrop)
                .child(icon),
        )
        .into_any_element()
}

/// How full the context is, as a small ring with the percentage in its
/// color, drawn on `background`.
pub(crate) fn render_context_meter(fraction: f64, background: Hsla, cx: &App) -> AnyElement {
    let color = context_hsla(fraction, background, cx);
    let percent = (fraction * 100.).round() as u32;
    h_flex()
        .debug_selector(|| "agent-tab-context".into())
        .flex_none()
        .gap(px(3.))
        .child(
            CircularProgress::new(fraction.min(1.) as f32, 1., px(14.), cx)
                .stroke_width(px(2.2))
                .radius(px(5.5))
                .bg_color(cx.theme().colors().border)
                .progress_color(color),
        )
        .child(
            Label::new(format!("{percent}%"))
                .untranslated()
                .size(LabelSize::Custom(rems_from_px(11_f32)))
                .color(Color::Custom(color)),
        )
        .into_any_element()
}

/// A bar `fraction` full in the context's color, drawn on `background`.
pub(crate) fn render_context_bar(
    fraction: f64,
    width: impl Into<gpui::Length>,
    height: Pixels,
    background: Hsla,
    cx: &App,
) -> AnyElement {
    div()
        .flex_none()
        .w(width.into())
        .h(height)
        .rounded(height / 2.)
        .overflow_hidden()
        .bg(cx.theme().colors().border)
        .child(
            div()
                .h_full()
                .w(relative(fraction.clamp(0., 1.) as f32))
                .bg(context_hsla(fraction, background, cx)),
        )
        .into_any_element()
}

/// What the card shown on an agent terminal's tab says about the agent.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct AgentCard {
    /// The agent's name as it ran (`claude`), for its mark.
    pub agent: String,
    pub status: Option<AgentStatus>,
    /// The agent and its model: "Claude Code · claude-opus-5-5".
    pub agent_line: String,
    pub session_title: Option<String>,
    /// Whether the session was only guessed from the directory.
    pub guessed: bool,
    pub context: Option<ContextLine>,
    pub turn: TokenUsage,
    pub session: TokenUsage,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ContextLine {
    /// "542.0k / 1.0M · 54%".
    pub text: String,
    pub fraction: Option<f64>,
}

impl AgentCard {
    /// The card for agent `name`, from its session's records when found.
    pub(crate) fn new(
        name: &str,
        status: Option<AgentStatus>,
        session: Option<(&SessionState, MatchedBy)>,
    ) -> Self {
        let mut card = AgentCard {
            agent: name.to_string(),
            status,
            agent_line: name.to_string(),
            ..Default::default()
        };
        let Some((state, matched_by)) = session else {
            return card;
        };
        let usage = &state.usage;
        card.agent_line = state.agent().name().to_string();
        if let Some(model) = usage.model.as_deref().filter(|model| !model.is_empty()) {
            card.agent_line.push_str(&format!(" · {model}"));
        }
        card.session_title = state.title.clone();
        card.guessed = matches!(matched_by, MatchedBy::Recent { ambiguous: true });
        card.context = usage.context_tokens.map(|tokens| {
            let fraction = usage.context_fraction();
            let text = match (usage.context_window, fraction) {
                (Some(window), Some(fraction)) => format!(
                    "{} / {} · {:.0}%",
                    format_tokens(tokens),
                    format_tokens(window),
                    fraction * 100.
                ),
                _ => format_tokens(tokens),
            };
            ContextLine { text, fraction }
        });
        card.turn = usage.turn;
        card.session = usage.session;
        card
    }

    /// The line on what of the session's input the cache served.
    pub(crate) fn cache_line(&self) -> Option<String> {
        let session = &self.session;
        if session.cache_read == 0 && session.cache_creation == 0 {
            return None;
        }
        let (read, written) = (
            format_tokens(session.cache_read),
            format_tokens(session.cache_creation),
        );
        Some(if util::i18n::is_chinese() {
            format!("本会话输入里 {read} 是缓存读取，{written} 是缓存写入")
        } else {
            format!(
                "Of the session's input, {read} was read from the cache and {written} written to it"
            )
        })
    }
}

/// What the card on a terminal's tab shows.
pub(crate) struct TabCard {
    /// The tab's title, for a terminal without an agent.
    pub title: String,
    pub agent: Option<AgentCard>,
    /// Where it runs: (what, value) for its directory, branch and command.
    pub location: Vec<(&'static str, String)>,
    /// A note under the row of `location` with this value (or the first):
    /// (value, note).
    pub location_note: Option<(String, String)>,
    /// A warning, like the terminal not outliving Solo.
    pub warning: Option<&'static str>,
    pub pid: Option<u32>,
}

const LABEL_COLUMN: f32 = 64.;

impl TabCard {
    pub(crate) fn render(&self, cx: &App) -> AnyElement {
        let small = |text: String| Label::new(text).size(LabelSize::Small);
        let mut card = v_flex().max_w(rems_from_px(420_f32)).gap_3().py_1();

        // Who and what it's doing.
        let header = match &self.agent {
            Some(agent) => {
                let title = agent
                    .session_title
                    .clone()
                    .unwrap_or_else(|| self.title.clone());
                v_flex()
                    .gap_1p5()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(render_agent_mark(&agent.agent, cx))
                            .child(
                                Label::new(title)
                                    .untranslated()
                                    .weight(FontWeight::BOLD)
                                    .truncate(),
                            )
                            .when(agent.guessed, |this| {
                                this.child(
                                    small(tr("(guessed)", "（推测）").to_string())
                                        .color(Color::Muted),
                                )
                            }),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .when_some(agent.status, |this, status| {
                                this.child(
                                    h_flex()
                                        .flex_none()
                                        .gap_1()
                                        .px_2()
                                        .rounded_full()
                                        .border_1()
                                        .border_color(cx.theme().colors().border)
                                        .child(
                                            AgentStateIcon::new(status)
                                                .size(IconSize::Custom(rems_from_px(10_f32))),
                                        )
                                        .child(small(status.label().to_string())),
                                )
                            })
                            .child(
                                small(agent.agent_line.clone())
                                    .untranslated()
                                    .color(Color::Muted)
                                    .truncate(),
                            ),
                    )
            }
            None => v_flex().child(Label::new(self.title.clone()).untranslated()),
        };
        card = card.child(header);

        if let Some(warning) = self.warning {
            card = card.child(small(warning.to_string()).color(Color::Warning));
        }

        // How full the context is.
        if let Some(context) = self.agent.as_ref().and_then(|agent| agent.context.as_ref()) {
            card = card.child(
                v_flex()
                    .gap_1p5()
                    .child(
                        h_flex()
                            .justify_between()
                            .gap_4()
                            .child(small(tr("Context", "上下文").to_string()).color(Color::Muted))
                            .child(small(context.text.clone()).untranslated().buffer_font(cx)),
                    )
                    .when_some(context.fraction, |this, fraction| {
                        this.child(render_context_bar(
                            fraction,
                            relative(1.),
                            px(6.),
                            cx.theme().colors().elevated_surface_background,
                            cx,
                        ))
                    }),
            );
        }

        // Where it runs.
        if !self.location.is_empty() {
            let note_row = self.location_note.as_ref().map(|(under, _)| {
                self.location
                    .iter()
                    .position(|(_, value)| value == under)
                    .unwrap_or(0)
            });
            card = card.child(
                v_flex()
                    .gap_1()
                    .children(self.location.iter().enumerate().map(|(ix, (what, value))| {
                        let note = self
                            .location_note
                            .as_ref()
                            .filter(|_| note_row == Some(ix))
                            .map(|(_, note)| note.clone());
                        h_flex()
                            .gap_2()
                            .items_start()
                            .child(
                                div()
                                    .flex_none()
                                    .w(rems_from_px(LABEL_COLUMN))
                                    .child(small(what.to_string()).color(Color::Muted)),
                            )
                            .child(
                                v_flex()
                                    .min_w_0()
                                    .child(
                                        small(value.clone())
                                            .untranslated()
                                            .buffer_font(cx)
                                            .truncate(),
                                    )
                                    .when_some(note, |this, note| {
                                        this.child(small(note).untranslated().color(Color::Muted))
                                    }),
                            )
                    })),
            );
        }

        // Its tokens.
        if let Some(agent) = self
            .agent
            .as_ref()
            .filter(|agent| !agent.session.is_empty())
        {
            let row = |what: String, input: String, output: String, color: Color, mono: bool| {
                let value = |text: String| {
                    div().flex_1().flex().justify_end().child(
                        small(text)
                            .untranslated()
                            .color(color)
                            .when(mono, |label| label.buffer_font(cx)),
                    )
                };
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_none()
                            .w(rems_from_px(LABEL_COLUMN))
                            .child(small(what).color(if mono { Color::Default } else { color })),
                    )
                    .child(value(input))
                    .child(value(output))
            };
            card = card
                .child(Divider::horizontal())
                .child(
                    v_flex()
                        .gap_1()
                        .child(row(
                            "token".to_string(),
                            tr("Input", "输入").to_string(),
                            tr("Output", "输出").to_string(),
                            Color::Muted,
                            false,
                        ))
                        .child(row(
                            tr("Turn", "本轮").to_string(),
                            format_tokens(agent.turn.total_input()),
                            format_tokens(agent.turn.output),
                            Color::Default,
                            true,
                        ))
                        .child(row(
                            tr("Session", "本会话").to_string(),
                            format_tokens(agent.session.total_input()),
                            format_tokens(agent.session.output),
                            Color::Default,
                            true,
                        )),
                )
                .when_some(agent.cache_line(), |this, line| {
                    this.child(small(line).untranslated().color(Color::Muted))
                });
        }

        if let Some(pid) = self.pid {
            card = card.child(Divider::horizontal()).child(
                small(format!("{}{pid}", tr("Process ", "进程 ")))
                    .untranslated()
                    .color(Color::Muted),
            );
        }
        card.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_sessions::{AgentAdapter as _, AgentKind, ClaudeCode};

    #[test]
    fn test_format_tokens() {
        assert_eq!(format_tokens(950), "950");
        assert_eq!(format_tokens(12_345), "12.3k");
        assert_eq!(format_tokens(4_520_000), "4.5M");
    }

    #[test]
    fn test_format_tokens_short() {
        assert_eq!(format_tokens_short(0), "0");
        assert_eq!(format_tokens_short(482), "482");
        assert_eq!(format_tokens_short(1_500), "1.5k");
        assert_eq!(format_tokens_short(12_345), "12.3k");
        assert_eq!(format_tokens_short(542_000), "542k");
        assert_eq!(format_tokens_short(999_700), "1M");
        assert_eq!(format_tokens_short(1_000_000), "1M");
        assert_eq!(format_tokens_short(1_100_000), "1.1M");
        assert_eq!(format_tokens_short(1_050_000), "1.05M");
        assert_eq!(format_tokens_short(211_400_000), "211M");
        assert_eq!(format_tokens_short(200_000), "200k");
    }

    #[test]
    fn test_short_model() {
        assert_eq!(short_model("claude-opus-5-5"), "opus-5-5");
        assert_eq!(short_model("claude-haiku-4-5-20251001"), "haiku-4-5");
        assert_eq!(short_model("gpt-5.1-codex"), "gpt-5.1-codex");
    }

    #[test]
    fn test_context_color() {
        assert_eq!(context_color(0.5), Color::Muted);
        assert_eq!(context_color(0.8), Color::Warning);
        assert_eq!(context_color(0.949), Color::Warning);
        assert_eq!(context_color(0.95), Color::Error);
    }

    #[test]
    fn test_agent_card() {
        let card = AgentCard::new("aider", Some(AgentStatus::Idle), None);
        assert_eq!(card.agent_line, "aider");
        assert_eq!(card.context, None);
        assert!(card.session.is_empty() && card.cache_line().is_none());

        let mut state = SessionState::new(AgentKind::ClaudeCode);
        for line in [
            r#"{"type":"user","message":{"content":"Go"}}"#,
            r#"{"type":"assistant","message":{"model":"claude-haiku-4-5","id":"m1","content":[],"usage":{"input_tokens":10,"output_tokens":2000,"cache_read_input_tokens":150000,"cache_creation_input_tokens":20000}}}"#,
            r#"{"type":"ai-title","aiTitle":"Fix the build"}"#,
        ] {
            state.apply_all(ClaudeCode.parse_line(line));
        }
        let card = AgentCard::new(
            "claude",
            Some(AgentStatus::Working),
            Some((&state, MatchedBy::Recent { ambiguous: true })),
        );
        assert_eq!(card.agent_line, "Claude Code · claude-haiku-4-5");
        assert_eq!(card.session_title.as_deref(), Some("Fix the build"));
        assert!(card.guessed);
        let context = card.context.clone().expect("context");
        assert_eq!(context.text, "170.0k / 200.0k · 85%");
        assert_eq!(context_color(context.fraction.unwrap()), Color::Warning);
        assert_eq!(card.session.total_input(), 170_010);
        assert_eq!(card.turn.output, 2_000);
        assert_eq!(
            card.cache_line().unwrap(),
            util::i18n::tr(
                "Of the session's input, 150.0k was read from the cache and 20.0k written to it",
                "本会话输入里 150.0k 是缓存读取，20.0k 是缓存写入"
            )
        );
    }
}
