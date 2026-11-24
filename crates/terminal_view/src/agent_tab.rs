//! SOLO: how an agent's terminal tab shows the agent: its icon with its
//! status on it, how full its context is, and the card shown on hover.

use std::{
    rc::Rc,
    time::{Duration, Instant},
};

use agent_sessions::{
    CONTEXT_ALMOST_OUT, CONTEXT_NEARLY_FULL, MatchedBy, ResponseSummary, SessionState, TokenUsage,
};
use gpui::{AnyElement, App, FontWeight, Hsla, Window};
use ui::{CircularProgress, Divider, prelude::*};
use util::i18n::tr;
use workspace::agent_activity::{AgentStateIcon, legible_on};

use crate::{
    TerminalView,
    agent_session::AgentMetrics,
    agent_status::{AgentStatus, HooksMissing},
};

impl TerminalView {
    /// The card shown on the terminal's tab; `None` before its process runs.
    pub(crate) fn tab_card(&self, cx: &App) -> Option<TabCard> {
        let terminal = self.terminal().read(cx);
        let pid = terminal.pid_getter()?.fallback_pid().as_u32();
        let agent = self.agent_status().map(|(name, status)| {
            let session = self.agent_session().map(|session| session.read(cx));
            AgentCard {
                hooks_missing: self.agent_hooks_missing(Instant::now()),
                ..AgentCard::new(
                    name,
                    Some(status),
                    session.map(|session| (session.state(), session.location().matched_by)),
                    &self.agent_metrics(cx),
                )
            }
        });
        // SOLO: the note on an agent without Solo's hooks opens "why this
        // status".
        let explain = agent
            .as_ref()
            .is_some_and(|agent| agent.hooks_missing.is_some())
            .then(|| {
                let view = self.self_handle.clone();
                Rc::new(move |_: &mut Window, cx: &mut App| {
                    view.update(cx, |view, cx| view.show_agent_status_explanation(cx))
                        .ok();
                }) as Rc<dyn Fn(&mut Window, &mut App)>
            });
        Some(TabCard {
            explain,
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
            warning: terminal
                .host_failed()
                .then(|| tr("Ends when Solo quits", "Solo 退出时会结束")),
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

/// SOLO: a time to first token: "5.1 s".
pub(crate) fn format_first_token(first_token: Duration) -> String {
    format!("{:.1} s", first_token.as_secs_f64())
}

/// SOLO: an output speed: "128 token/s".
pub(crate) fn format_speed(tokens_per_second: f64) -> String {
    format!("{tokens_per_second:.0} token/s")
}

/// SOLO: how long an agent worked: "0:42", "2:14", "1:12:05".
pub(crate) fn format_work_time(time: Duration) -> String {
    let seconds = time.as_secs();
    let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
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

/// Claude's orange, for Claude Code's logo.
const CLAUDE_ORANGE: u32 = 0xD97757;

/// The agent's logo, Zed's for the agents it has one for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AgentLogo {
    icon: IconName,
    /// Its brand color; `None` for a monochrome logo, drawn in the text color.
    color: Option<u32>,
}

fn agent_logo(name: &str) -> Option<AgentLogo> {
    let name = name.to_lowercase();
    let (icon, color) = if name.starts_with("claude") {
        (IconName::AiClaude, Some(CLAUDE_ORANGE))
    } else if name.starts_with("codex") {
        (IconName::AiOpenAi, None)
    } else if name.starts_with("copilot") {
        (IconName::Copilot, None)
    } else if name.starts_with("gemini") {
        (IconName::AiGemini, None)
    } else if name.starts_with("opencode") {
        (IconName::AiOpenCode, None)
    } else if name.starts_with("grok") {
        (IconName::AiXAi, None)
    } else {
        return None;
    };
    Some(AgentLogo { icon, color })
}

/// The color `name`'s logo is drawn in on `background`: its brand color, or
/// the text color for a monochrome logo, kept legible there.
fn agent_logo_color(logo: AgentLogo, background: Hsla, cx: &App) -> Hsla {
    let color = match logo.color {
        Some(color) => Hsla::from(gpui::rgb(color)),
        None => cx.theme().colors().text,
    };
    legible_on(color, background)
}

/// The logo `name` has and the color it's drawn in on `background`, for a
/// menu entry's icon.
pub(crate) fn agent_logo_icon(name: &str, background: Hsla, cx: &App) -> Option<(IconName, Hsla)> {
    let logo = agent_logo(name)?;
    Some((logo.icon, agent_logo_color(logo, background, cx)))
}

/// The agent's logo in its own color on `background` (what it's drawn on),
/// or its initial in a small square when it has none.
pub(crate) fn render_agent_mark(name: &str, background: Hsla, cx: &App) -> AnyElement {
    let mark = div()
        .flex_none()
        .size(rems_from_px(16_f32))
        .flex()
        .items_center()
        .justify_center();
    match agent_logo(name) {
        Some(logo) => mark
            .debug_selector(move || format!("agent-logo-{:?}", logo.icon))
            .child(
                Icon::new(logo.icon)
                    .size(IconSize::Custom(rems_from_px(15_f32)))
                    .color(Color::Custom(agent_logo_color(logo, background, cx))),
            ),
        None => {
            let initial = name
                .chars()
                .find(|c| c.is_alphanumeric())
                .map(|c| c.to_uppercase().to_string())
                .unwrap_or_else(|| "?".to_string());
            mark.debug_selector(|| "agent-initial".into())
                .rounded(px(4.))
                .bg(cx.theme().colors().border)
                .child(
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
        .child(render_agent_mark(name, backdrop, cx))
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
    /// The agent, its model and its effort: "Claude Code · claude-opus-5-5
    /// · xhigh".
    pub agent_line: String,
    pub session_title: Option<String>,
    /// Whether the session was only guessed from the directory.
    pub guessed: bool,
    pub context: Option<ContextLine>,
    pub turn: TokenUsage,
    pub session: TokenUsage,
    /// SOLO: how long the agent worked in the turn and in the session.
    pub turn_time: Option<Duration>,
    pub session_time: Option<Duration>,
    /// SOLO: how fast the model answered.
    pub responses: ResponseSummary,
    /// SOLO: the agent can report its status through Solo's hooks but runs
    /// without them.
    pub hooks_missing: Option<HooksMissing>,
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
        metrics: &AgentMetrics,
    ) -> Self {
        let mut card = AgentCard {
            agent: name.to_string(),
            status,
            agent_line: name.to_string(),
            turn_time: metrics.turn_time,
            session_time: metrics.session_time,
            responses: metrics.responses,
            ..Default::default()
        };
        let effort = metrics
            .effort
            .as_deref()
            .map(|effort| format!(" · {effort}"))
            .unwrap_or_default();
        let Some((state, matched_by)) = session else {
            card.agent_line.push_str(&effort);
            return card;
        };
        let usage = &state.usage;
        card.agent_line = state.agent().name().to_string();
        if let Some(model) = usage.model.as_deref().filter(|model| !model.is_empty()) {
            card.agent_line.push_str(&format!(" · {model}"));
        }
        card.agent_line.push_str(&effort);
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

    /// SOLO: the note under the status of an agent that runs without
    /// Solo's hooks.
    pub(crate) fn hooks_note(&self) -> Option<&'static str> {
        self.hooks_missing.map(|_| {
            tr(
                "No status reports — read from screen. A new terminal fixes it.",
                "收不到状态上报，改为读屏幕。新开一个终端即可恢复。",
            )
        })
    }

    /// SOLO: the note by the turn's figures when the agent's turns won't
    /// list the files they change: its hooks' files were missing.
    pub(crate) fn unlisted_files_note(&self) -> Option<&'static str> {
        matches!(self.hooks_missing, Some(HooksMissing::FilesMissing)).then(|| {
            tr(
                "Changed files won't be listed for this session",
                "本会话不会列出改动的文件",
            )
        })
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
    /// SOLO: shows why the agent has its status, for the note on an agent
    /// without Solo's hooks.
    pub explain: Option<Rc<dyn Fn(&mut Window, &mut App)>>,
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
                            .child(render_agent_mark(
                                &agent.agent,
                                cx.theme().colors().elevated_surface_background,
                                cx,
                            ))
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
                                        .child(small(status.label().to_string()))
                                        .when(agent.hooks_missing.is_some(), |this| {
                                            this.child(
                                                Icon::new(IconName::Warning)
                                                    .size(IconSize::Custom(rems_from_px(10_f32)))
                                                    .color(Color::Warning),
                                            )
                                        }),
                                )
                            })
                            .child(
                                small(agent.agent_line.clone())
                                    .untranslated()
                                    .color(Color::Muted)
                                    .truncate(),
                            ),
                    )
                    // SOLO: an agent without Solo's hooks says so; the note
                    // opens "why this status".
                    .when_some(agent.hooks_note(), |this, note| {
                        let explain = self.explain.clone();
                        this.child(
                            div()
                                .id("agent-hooks-missing")
                                .debug_selector(|| "agent-hooks-missing".into())
                                .when_some(explain, |this, explain| {
                                    this.cursor_pointer()
                                        .on_click(move |_, window, cx| explain(window, cx))
                                })
                                .child(small(note.to_string()).color(Color::Muted)),
                        )
                    })
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

        // Its tokens and time.
        if let Some(agent) = self.agent.as_ref() {
            let has_tokens = !agent.session.is_empty();
            let has_time = agent.turn_time.is_some() || agent.session_time.is_some();
            let row = |what: String, values: Vec<String>, color: Color, mono: bool| {
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
                            .min_w(rems_from_px(LABEL_COLUMN))
                            .child(small(what).color(if mono { Color::Default } else { color })),
                    )
                    .children(values.into_iter().map(value))
            };
            let time = |time: Option<Duration>| time.map(format_work_time).unwrap_or_default();
            if has_tokens || has_time {
                let columns = |input: String, output: String, time: String| {
                    let mut values = Vec::new();
                    if has_tokens {
                        values.extend([input, output]);
                    }
                    if has_time {
                        values.push(time);
                    }
                    values
                };
                card = card
                    .child(Divider::horizontal())
                    .child(
                        v_flex()
                            .gap_1()
                            .child(row(
                                if has_tokens {
                                    "token".to_string()
                                } else {
                                    String::new()
                                },
                                columns(
                                    tr("Input", "输入").to_string(),
                                    tr("Output", "输出").to_string(),
                                    tr("Time", "用时").to_string(),
                                ),
                                Color::Muted,
                                false,
                            ))
                            .child(row(
                                tr("Turn", "本轮").to_string(),
                                columns(
                                    format_tokens(agent.turn.total_input()),
                                    format_tokens(agent.turn.output),
                                    time(agent.turn_time),
                                ),
                                Color::Default,
                                true,
                            ))
                            .child(row(
                                tr("Session", "本会话").to_string(),
                                columns(
                                    format_tokens(agent.session.total_input()),
                                    format_tokens(agent.session.output),
                                    time(agent.session_time),
                                ),
                                Color::Default,
                                true,
                            )),
                    )
                    .when_some(agent.cache_line(), |this, line| {
                        this.child(small(line).untranslated().color(Color::Muted))
                    });
            }
            // SOLO: no turn will list its changed files.
            if let Some(note) = agent.unlisted_files_note() {
                card = card.child(
                    div()
                        .debug_selector(|| "agent-files-unlisted".into())
                        .child(small(note.to_string()).color(Color::Muted)),
                );
            }

            // How fast the model answered.
            let responses = &agent.responses;
            let first_token = |first_token: Option<Duration>| {
                first_token.map(format_first_token).unwrap_or_default()
            };
            let speed = |speed: Option<f64>| speed.map(format_speed).unwrap_or_default();
            let latest = (responses.latest_first_token.is_some()
                || responses.latest_speed.is_some())
            .then(|| {
                row(
                    tr("Latest", "最近").to_string(),
                    vec![
                        first_token(responses.latest_first_token),
                        speed(responses.latest_speed),
                    ],
                    Color::Default,
                    true,
                )
            });
            let mean = (responses.mean_first_token.is_some() || responses.mean_speed.is_some())
                .then(|| {
                    row(
                        tr("Session average", "本会话平均").to_string(),
                        vec![
                            first_token(responses.mean_first_token),
                            speed(responses.mean_speed),
                        ],
                        Color::Default,
                        true,
                    )
                });
            if latest.is_some() || mean.is_some() {
                card = card.child(Divider::horizontal()).child(
                    v_flex()
                        .gap_1()
                        .child(row(
                            tr("Response", "响应").to_string(),
                            vec![
                                tr("First token", "首字").to_string(),
                                tr("Speed", "速度").to_string(),
                            ],
                            Color::Muted,
                            false,
                        ))
                        .children(latest)
                        .children(mean),
                );
            }
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
    fn test_agent_logo() {
        let logo = |name| agent_logo(name).map(|logo| (logo.icon, logo.color));
        assert_eq!(
            logo("Claude Code"),
            Some((IconName::AiClaude, Some(CLAUDE_ORANGE)))
        );
        assert_eq!(
            logo("claude"),
            Some((IconName::AiClaude, Some(CLAUDE_ORANGE)))
        );
        // Monochrome logos, drawn in the text color.
        assert_eq!(logo("codex"), Some((IconName::AiOpenAi, None)));
        assert_eq!(logo("Codex"), Some((IconName::AiOpenAi, None)));
        assert_eq!(logo("copilot"), Some((IconName::Copilot, None)));
        assert_eq!(logo("gemini"), Some((IconName::AiGemini, None)));
        assert_eq!(logo("Gemini CLI"), Some((IconName::AiGemini, None)));
        assert_eq!(logo("opencode"), Some((IconName::AiOpenCode, None)));
        assert_eq!(logo("grok"), Some((IconName::AiXAi, None)));
        // The rest show their initial.
        assert_eq!(logo("aider"), None);
        assert_eq!(logo("cursor-agent"), None);
    }

    #[test]
    fn test_claude_orange_contrast() {
        use gpui::rgb;
        use ui::utils::calculate_contrast_ratio;
        let orange = Hsla::from(rgb(CLAUDE_ORANGE));
        // Ayu Dark's active and inactive tabs: left as it is.
        for tab in [0x0d1016, 0x1f2127] {
            let tab = Hsla::from(rgb(tab));
            assert!(calculate_contrast_ratio(orange, tab) >= 3.);
            assert_eq!(legible_on(orange, tab), orange);
        }
        // One Light's: too pale, darkened, still orange.
        for tab in [0xfafafa, 0xebebec] {
            let tab = Hsla::from(rgb(tab));
            assert!(calculate_contrast_ratio(orange, tab) < 3.);
            let legible = legible_on(orange, tab);
            assert!(calculate_contrast_ratio(legible, tab) >= 3.);
            assert!(legible.l < orange.l && (legible.h - orange.h).abs() < 0.01);
        }
    }

    #[gpui::test]
    async fn test_agent_mark_square_only_for_initials(cx: &mut gpui::TestAppContext) {
        struct Mark(&'static str);
        impl Render for Mark {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                div().child(render_agent_mark(
                    self.0,
                    cx.theme().colors().tab_active_background,
                    cx,
                ))
            }
        }
        crate::tests::init_test(cx).await;
        let (mark, cx) = cx.add_window_view(|_, _| Mark("claude"));
        cx.run_until_parked();
        assert!(cx.debug_bounds("agent-logo-AiClaude").is_some());
        assert!(cx.debug_bounds("agent-initial").is_none());
        mark.update(cx, |mark, cx| {
            mark.0 = "aider";
            cx.notify();
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("agent-logo-AiClaude").is_none());
        assert!(cx.debug_bounds("agent-initial").is_some());
    }

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
    fn test_format_speeds_and_times() {
        assert_eq!(format_first_token(Duration::from_millis(5_140)), "5.1 s");
        assert_eq!(format_first_token(Duration::from_millis(860)), "0.9 s");
        assert_eq!(format_speed(127.6), "128 token/s");
        assert_eq!(format_work_time(Duration::from_secs(42)), "0:42");
        assert_eq!(format_work_time(Duration::from_millis(134_900)), "2:14");
        assert_eq!(format_work_time(Duration::from_secs(4325)), "1:12:05");
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
        let card = AgentCard::new(
            "aider",
            Some(AgentStatus::Idle),
            None,
            &AgentMetrics::default(),
        );
        assert_eq!(card.agent_line, "aider");
        assert_eq!(card.turn_time, None);
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
        let metrics = AgentMetrics {
            effort: Some("xhigh".to_string()),
            turn_time: Some(Duration::from_secs(134)),
            ..Default::default()
        };
        let card = AgentCard::new(
            "claude",
            Some(AgentStatus::Working),
            Some((&state, MatchedBy::Recent { ambiguous: true })),
            &metrics,
        );
        assert_eq!(card.agent_line, "Claude Code · claude-haiku-4-5 · xhigh");
        assert_eq!(card.turn_time, Some(Duration::from_secs(134)));
        // Without a session, the effort the agent reported still shows.
        let no_session = AgentCard::new("opencode", None, None, &metrics);
        assert_eq!(no_session.agent_line, "opencode · xhigh");
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

    #[test]
    fn test_hooks_notes() {
        let card = |hooks_missing| AgentCard {
            agent: "claude".to_string(),
            status: Some(AgentStatus::Working),
            hooks_missing,
            ..Default::default()
        };
        let with_hooks = card(None);
        assert_eq!(with_hooks.hooks_note(), None);
        assert_eq!(with_hooks.unlisted_files_note(), None);
        // Only quiet so far: its status is read from the screen, but that
        // its turns won't list files isn't certain.
        let quiet = card(Some(HooksMissing::NoReports(Duration::from_secs(20))));
        assert_eq!(
            quiet.hooks_note(),
            Some(tr(
                "No status reports — read from screen. A new terminal fixes it.",
                "收不到状态上报，改为读屏幕。新开一个终端即可恢复。",
            ))
        );
        assert_eq!(quiet.unlisted_files_note(), None);
        let files_missing = card(Some(HooksMissing::FilesMissing));
        assert_eq!(files_missing.hooks_note(), quiet.hooks_note());
        assert_eq!(
            files_missing.unlisted_files_note(),
            Some(tr(
                "Changed files won't be listed for this session",
                "本会话不会列出改动的文件",
            ))
        );
    }

    #[gpui::test]
    async fn test_card_hooks_note_opens_explanation(cx: &mut gpui::TestAppContext) {
        struct Card(TabCard);
        impl Render for Card {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                div().size_full().child(self.0.render(cx))
            }
        }
        let tab_card = |hooks_missing: Option<HooksMissing>, explain| TabCard {
            title: "claude".to_string(),
            agent: Some(AgentCard {
                agent: "claude".to_string(),
                status: Some(AgentStatus::Working),
                agent_line: "Claude Code".to_string(),
                hooks_missing,
                ..Default::default()
            }),
            location: Vec::new(),
            location_note: None,
            warning: None,
            pid: Some(42),
            explain,
        };
        crate::tests::init_test(cx).await;
        let explained = Rc::new(std::cell::Cell::new(0));
        let explain = {
            let explained = explained.clone();
            Rc::new(move |_: &mut Window, _: &mut App| explained.set(explained.get() + 1))
                as Rc<dyn Fn(&mut Window, &mut App)>
        };
        let (card, cx) = cx.add_window_view(|_, _| Card(tab_card(None, None)));
        cx.run_until_parked();
        assert!(cx.debug_bounds("agent-hooks-missing").is_none());
        assert!(cx.debug_bounds("agent-files-unlisted").is_none());

        card.update(cx, |card, cx| {
            card.0 = tab_card(
                Some(HooksMissing::NoReports(Duration::from_secs(20))),
                Some(explain.clone()),
            );
            cx.notify();
        });
        cx.run_until_parked();
        let note = cx.debug_bounds("agent-hooks-missing").expect("the note");
        assert!(cx.debug_bounds("agent-files-unlisted").is_none());
        cx.simulate_click(note.center(), gpui::Modifiers::none());
        assert_eq!(explained.get(), 1);

        card.update(cx, |card, cx| {
            card.0 = tab_card(Some(HooksMissing::FilesMissing), Some(explain.clone()));
            cx.notify();
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("agent-hooks-missing").is_some());
        assert!(cx.debug_bounds("agent-files-unlisted").is_some());
    }
}
