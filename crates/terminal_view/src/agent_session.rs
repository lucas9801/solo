//! SOLO: the session of the AI agent running in a terminal, read from the
//! agent's own records (see the `agent_sessions` crate) rather than from the
//! screen.
//!
//! The session is found, surest first, by the session id the agent's hook
//! reported, by the agent process below the terminal's shell, or as the one in
//! the terminal's directory written to since the agent started. When that
//! last guess is ambiguous, "Choose Agent Session" lets the user pick.

use std::{
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};

use agent_sessions::{
    AgentAdapter, AgentSession, AgentSessionEvent, MatchedBy, SessionLocation, SessionState,
    TerminalProbe,
};
use gpui::{
    App, AppContext as _, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable,
    Stateful, Subscription, Task, WeakEntity, Window, actions,
};
use picker::{Picker, PickerDelegate};
use terminal::shell_integration::AgentSessionId;
use ui::{ListItem, ListItemSpacing, prelude::*};
use workspace::ModalView;

use crate::{
    TerminalView,
    agent_tab::{
        context_color, context_hsla, format_tokens_short, render_context_bar, short_model,
    },
};

actions!(
    terminal,
    [
        /// Picks which of its agent's recorded sessions the terminal runs, for
        /// when Solo can't tell.
        ChooseAgentSession,
    ]
);

/// How long to wait before looking for a session again after not finding one.
const RETRY_AFTER: Duration = Duration::from_secs(5);

/// The agent session a terminal runs.
#[derive(Default)]
pub(crate) struct AgentSessionBinding {
    session: Option<Entity<AgentSession>>,
    /// When the agent run the session belongs to started.
    run_started_at: Option<Instant>,
    /// The session id the agent's hook last reported.
    reported: Option<AgentSessionId>,
    /// The session the user picked for this run.
    chosen: Option<SessionLocation>,
    last_attempt: Option<Instant>,
    finding: bool,
    _subscription: Option<Subscription>,
}

/// The agent a terminal runs or last ran, and what's known to find its session.
struct AgentRun {
    adapter: &'static dyn AgentAdapter,
    probe: TerminalProbe,
    started_at: Option<Instant>,
}

impl TerminalView {
    /// The session of the agent this terminal runs or last ran, once found.
    /// Look for it with [`Self::find_agent_session`].
    pub fn agent_session(&self) -> Option<&Entity<AgentSession>> {
        self.agent_session.session.as_ref()
    }

    /// SOLO: the usage of the agent running in this terminal, when it's to
    /// be shown outside the tooltip (`agent_usage_display`).
    fn shown_agent_usage(&self, cx: &App) -> Option<agent_sessions::UsageSummary> {
        use settings::Settings as _;
        let display =
            terminal::terminal_settings::TerminalSettings::get_global(cx).agent_usage_display;
        if display != terminal::terminal_settings::AgentUsageDisplay::Always {
            return None;
        }
        let (_, status) = self.agent_status()?;
        if !status.is_running() {
            return None;
        }
        Some(self.agent_session()?.read(cx).state().usage.clone())
    }

    /// SOLO: the thin bar at the bottom of an agent's terminal with its
    /// context and token usage (see [`usage_bar`]).
    pub(crate) fn render_agent_usage_bar(&self, cx: &App) -> Option<AnyElement> {
        let usage = self.shown_agent_usage(cx)?;
        let contents = UsageBar::new(&usage)?;
        Some(usage_bar(&contents, cx).into_any_element())
    }

    /// SOLO: how full the running agent's context is, for its tab.
    pub(crate) fn agent_context_fraction(&self, cx: &App) -> Option<f64> {
        self.shown_agent_usage(cx)?.context_fraction()
    }

    /// Whether an AI agent runs or ran in this terminal, so it may have a
    /// session.
    pub(crate) fn has_run_agent(&self, cx: &App) -> bool {
        self.agent_session.session.is_some() || self.agent_run(cx).is_some()
    }

    fn agent_run(&self, cx: &App) -> Option<AgentRun> {
        let terminal = self.terminal.read(cx);
        let block = terminal
            .shell_integration()
            .blocks()
            .iter()
            .rev()
            .filter(|block| block.started_at.is_some())
            .find(|block| {
                block
                    .command
                    .as_deref()
                    .is_some_and(|command| agent_sessions::adapter_for_command(command).is_some())
            });
        let (command, started_at, working_directory) = match block {
            Some(block) => (
                block.command.clone()?,
                block.started_at,
                block.working_directory.clone(),
            ),
            // Without shell integration, go by the foreground process.
            None => (terminal.foreground_process_command_name()?, None, None),
        };
        let adapter = agent_sessions::adapter_for_command(&command)?;
        let probe = TerminalProbe {
            // SOLO: the agent's own, when it said (a resumed session can work
            // elsewhere), where its sessions are kept.
            working_directory: self
                .agent_reported_directory()
                .cloned()
                .or(working_directory)
                .or_else(|| terminal.working_directory()),
            command: Some(command),
            started_at: started_at
                .and_then(|started_at| SystemTime::now().checked_sub(started_at.elapsed())),
            shell_pid: terminal
                .pid_getter()
                .map(|pid_getter| pid_getter.fallback_pid().as_u32()),
            reported_session_id: self.agent_session.reported.map(|id| id.to_string()),
        };
        Some(AgentRun {
            adapter,
            probe,
            started_at,
        })
    }

    /// Finds the session of the agent this terminal runs or last ran, in the
    /// background. One matched for sure is kept for as long as the agent runs.
    pub fn find_agent_session(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Task<Option<Entity<AgentSession>>> {
        let Some(run) = self.agent_run(cx) else {
            return Task::ready(self.agent_session.session.clone());
        };
        let binding = &mut self.agent_session;
        if binding.run_started_at != run.started_at {
            binding.run_started_at = run.started_at;
            binding.chosen = None;
            if binding.session.as_ref().is_some_and(|session| {
                session.read(cx).location().matched_by != MatchedBy::Reported
            }) {
                binding.session = None;
                binding._subscription = None;
            }
        }
        if let Some(session) = &binding.session {
            let location = session.read(cx).location();
            let reported_elsewhere = binding
                .reported
                .is_some_and(|reported| reported.to_string() != location.session_id);
            if !matches!(location.matched_by, MatchedBy::Recent { .. }) && !reported_elsewhere {
                return Task::ready(Some(session.clone()));
            }
        }
        if let Some(chosen) = binding.chosen.clone() {
            return Task::ready(Some(self.set_agent_session(chosen, cx)));
        }
        binding.last_attempt = Some(Instant::now());
        binding.finding = true;
        let AgentRun { adapter, probe, .. } = run;
        let find = cx.background_spawn(async move { adapter.find_session(&probe) });
        cx.spawn(async move |this, cx| {
            let location = find.await;
            this.update(cx, |this, cx| {
                this.agent_session.finding = false;
                match location {
                    Some(location) => Some(this.set_agent_session(location, cx)),
                    None => this.agent_session.session.clone(),
                }
            })
            .ok()
            .flatten()
        })
    }

    fn set_agent_session(
        &mut self,
        location: SessionLocation,
        cx: &mut Context<Self>,
    ) -> Entity<AgentSession> {
        if let Some(session) = &self.agent_session.session
            && session.read(cx).location().path == location.path
            && session.read(cx).location().matched_by == location.matched_by
        {
            return session.clone();
        }
        log::debug!(
            "terminal agent session: {} ({:?})",
            location.session_id,
            location.matched_by
        );
        let session = cx.new(|cx| AgentSession::new(location, cx));
        self.agent_session._subscription = Some(cx.subscribe(&session, Self::agent_session_event));
        self.agent_session.session = Some(session.clone());
        session
    }

    /// The session changed: its usage shows on the tab and the usage bar.
    fn agent_session_event(
        &mut self,
        _: Entity<AgentSession>,
        _: &AgentSessionEvent,
        cx: &mut Context<Self>,
    ) {
        cx.notify();
    }

    /// The agent's hook reported its session id.
    pub(crate) fn agent_session_reported(
        &mut self,
        session_id: AgentSessionId,
        cx: &mut Context<Self>,
    ) {
        if self.agent_session.reported == Some(session_id) {
            return;
        }
        self.agent_session.reported = Some(session_id);
        self.find_agent_session(cx).detach();
    }

    /// Keeps looking for the session of a running agent until one is found
    /// for sure.
    pub(crate) fn keep_agent_session(&mut self, cx: &mut Context<Self>) {
        let binding = &self.agent_session;
        if binding.finding
            || binding
                .last_attempt
                .is_some_and(|attempt| attempt.elapsed() < RETRY_AFTER)
        {
            return;
        }
        let sure = binding.session.as_ref().is_some_and(|session| {
            !matches!(
                session.read(cx).location().matched_by,
                MatchedBy::Recent { .. }
            )
        });
        if !sure {
            self.find_agent_session(cx).detach();
        }
    }

    /// The text of the latest answer of the agent this terminal runs, as the
    /// agent recorded it.
    pub fn latest_agent_answer(&mut self, cx: &mut Context<Self>) -> Task<Option<String>> {
        let find = self.find_agent_session(cx);
        cx.spawn(async move |_, cx| {
            let session = find.await?;
            agent_answers(&session, 1, cx).await?.into_iter().next()
        })
    }

    pub(crate) fn choose_agent_session(
        &mut self,
        _: &ChooseAgentSession,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(run) = self.agent_run(cx) else {
            self.show_agent_session_toast(
                util::i18n::tr(
                    "No AI agent has run in this terminal",
                    "这个终端里没有运行过 AI agent",
                ),
                cx,
            );
            return;
        };
        let Some(working_directory) = run.probe.working_directory.clone() else {
            return;
        };
        let adapter = run.adapter;
        let current = self
            .agent_session
            .session
            .as_ref()
            .map(|session| session.read(cx).location().path.clone());
        let list = cx.background_spawn(async move {
            adapter
                .sessions_in(&working_directory)
                .into_iter()
                .map(|location| {
                    let description = adapter.describe_session(&location);
                    (location, description)
                })
                .collect::<Vec<_>>()
        });
        let terminal_view = cx.weak_entity();
        let workspace = self.workspace.clone();
        cx.spawn_in(window, async move |this, cx| {
            let sessions = list.await;
            if sessions.is_empty() {
                this.update(cx, |this, cx| {
                    this.show_agent_session_toast(
                        util::i18n::tr(
                            "The agent has no recorded sessions for this directory",
                            "这个目录下没有 agent 的会话记录",
                        ),
                        cx,
                    )
                })
                .ok();
                return;
            }
            workspace
                .update_in(cx, |workspace, window, cx| {
                    workspace.toggle_modal(window, cx, |window, cx| {
                        AgentSessionPicker::new(terminal_view, sessions, current, window, cx)
                    })
                })
                .ok();
        })
        .detach();
    }

    fn show_agent_session_toast(&self, message: &'static str, cx: &mut Context<Self>) {
        if let Some(workspace) = self.workspace.upgrade() {
            workspace.update(cx, |workspace, cx| {
                workspace.show_toast(
                    workspace::Toast::new(
                        workspace::notifications::NotificationId::unique::<ChooseAgentSession>(),
                        message,
                    )
                    .autohide(),
                    cx,
                )
            });
        }
    }

    fn agent_session_chosen(&mut self, location: SessionLocation, cx: &mut Context<Self>) {
        let location = SessionLocation {
            matched_by: MatchedBy::Chosen,
            ..location
        };
        self.agent_session.chosen = Some(location.clone());
        self.set_agent_session(location, cx);
        cx.emit(workspace::item::ItemEvent::UpdateTab);
        cx.notify();
    }
}

/// The texts of the main conversation's last `limit` assistant messages,
/// latest first: from the live session once it has read its transcript, or
/// else read here and now in the background.
pub(crate) async fn agent_answers(
    session: &Entity<AgentSession>,
    limit: usize,
    cx: &mut gpui::AsyncApp,
) -> Option<Vec<String>> {
    let answers = move |state: &SessionState| {
        state
            .assistant_messages_latest_first()
            .take(limit)
            .map(|message| message.text.clone())
            .collect::<Vec<_>>()
    };
    let (loaded, location) = session.read_with(cx, |session, _| {
        (
            session.is_loaded().then(|| answers(session.state())),
            session.location().clone(),
        )
    });
    if let Some(loaded) = loaded {
        return Some(loaded);
    }
    cx.background_spawn(async move { SessionState::load(&location).map(|state| answers(&state)) })
        .await
        .map_err(|error| log::debug!("failed to read the agent transcript: {error}"))
        .ok()
}

/// SOLO: what an agent's usage bar shows, its numbers to three significant
/// digits; the exact ones are on the tab's card.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct UsageBar {
    /// The context's tokens out of the window ("542k / 1M"), and how full
    /// it is.
    context: Option<(String, Option<f64>)>,
    model: Option<String>,
    /// The turn's and the session's tokens, in and out.
    turn: Option<(String, String)>,
    session: Option<(String, String)>,
}

impl UsageBar {
    fn new(usage: &agent_sessions::UsageSummary) -> Option<Self> {
        let context = usage.context_tokens.map(|tokens| {
            let text = match usage.context_window {
                Some(window) => format!(
                    "{} / {}",
                    format_tokens_short(tokens),
                    format_tokens_short(window)
                ),
                None => format_tokens_short(tokens),
            };
            (text, usage.context_fraction())
        });
        let tokens = |usage: &agent_sessions::TokenUsage| {
            (
                format_tokens_short(usage.total_input()),
                format_tokens_short(usage.output),
            )
        };
        let (turn, session) = if usage.session.is_empty() {
            (None, None)
        } else {
            (Some(tokens(&usage.turn)), Some(tokens(&usage.session)))
        };
        let bar = Self {
            context,
            model: usage
                .model
                .as_deref()
                .filter(|model| !model.is_empty())
                .map(|model| short_model(model).to_string()),
            turn,
            session,
        };
        (bar.context.is_some() || bar.model.is_some() || bar.session.is_some()).then_some(bar)
    }
}

/// SOLO: the usage bar: the context (a short bar, its percentage and
/// tokens), the model, the turn's tokens and the session's. When narrow,
/// whole parts go: the session's tokens first, then the turn's, then the
/// model; last, only the context's bar and percentage stay.
///
/// Parts that don't fit wrap to a second line, which is cut off. They come
/// after an empty first part, so even the context can wrap away; then the
/// short context drawn below them shows.
fn usage_bar(contents: &UsageBar, cx: &App) -> Stateful<Div> {
    let line_height = rems_from_px(22_f32);
    let background = cx.theme().colors().terminal_background;
    let muted = |text: &str| {
        Label::new(text.to_string())
            .untranslated()
            .single_line()
            .size(LabelSize::Small)
            .color(Color::Muted)
    };
    let number = |text: String| {
        Label::new(text)
            .untranslated()
            .single_line()
            .size(LabelSize::Small)
            .buffer_font(cx)
    };
    let part = |name: &'static str| {
        h_flex()
            .debug_selector(move || format!("agent-usage-{name}"))
            .flex_none()
            .h(line_height)
            .gap_2()
            .bg(background)
    };
    let meter = |fraction: f64| {
        let color = match context_color(fraction) {
            Color::Muted => Color::Default,
            _ => Color::Custom(context_hsla(fraction, background, cx)),
        };
        h_flex()
            .flex_none()
            .gap_2()
            .child(render_context_bar(
                fraction,
                px(72.),
                px(5.),
                background,
                cx,
            ))
            .child(number(format!("{:.0}%", fraction * 100.)).color(color))
    };
    let tokens = |name: &'static str, what: &str, (input, output): (String, String)| {
        part(name)
            .ml_3()
            .child(
                div()
                    .flex_none()
                    .w(px(1.))
                    .h(rems_from_px(14_f32))
                    .mr_1()
                    .bg(cx.theme().colors().border),
            )
            .child(muted(what))
            .child(muted(util::i18n::tr("in", "输入")))
            .child(number(input))
            .child(muted(util::i18n::tr("out", "输出")))
            .child(number(output))
    };

    let minimal = contents
        .context
        .as_ref()
        .and_then(|(_, fraction)| *fraction)
        .map(|fraction| part("minimal").child(meter(fraction)));
    let mut parts = div()
        .debug_selector(|| "agent-usage-parts".into())
        .flex()
        .flex_row()
        .flex_wrap()
        .w_full()
        .h(line_height)
        .overflow_hidden()
        .child(div().flex_none().w_0().h(line_height));
    if let Some((text, fraction)) = contents.context.clone() {
        parts = parts.child(
            part("context")
                .child(muted(util::i18n::tr("Context", "上下文")))
                .when_some(fraction, |this, fraction| this.child(meter(fraction)))
                .child(number(text).color(Color::Muted)),
        );
    }
    if let Some(model) = contents.model.clone() {
        parts = parts.child(
            part("model").ml_3().child(
                div()
                    .px_1p5()
                    .rounded_sm()
                    .border_1()
                    .border_color(cx.theme().colors().border)
                    .child(number(model)),
            ),
        );
    }
    if let Some(turn) = contents.turn.clone() {
        parts = parts.child(tokens("turn", util::i18n::tr("Turn", "本轮"), turn));
    }
    if let Some(session) = contents.session.clone() {
        parts = parts.child(tokens(
            "session",
            util::i18n::tr("Session", "本会话"),
            session,
        ));
    }
    div()
        .id("agent-usage-bar")
        .w_full()
        .px_3()
        .border_t_1()
        .border_color(cx.theme().colors().border_variant)
        .bg(background)
        .child(
            div()
                .relative()
                .w_full()
                .h(line_height)
                .overflow_hidden()
                .when_some(minimal, |this, minimal| {
                    this.child(div().absolute().top_0().left_0().child(minimal))
                })
                .child(parts),
        )
}

struct AgentSessionPicker {
    picker: Entity<Picker<AgentSessionPickerDelegate>>,
}

impl AgentSessionPicker {
    fn new(
        terminal_view: WeakEntity<TerminalView>,
        sessions: Vec<(SessionLocation, Option<String>)>,
        current: Option<std::path::PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let selected_index = sessions
            .iter()
            .position(|(location, _)| Some(&location.path) == current.as_ref())
            .unwrap_or(0);
        let delegate = AgentSessionPickerDelegate {
            picker: cx.entity().downgrade(),
            terminal_view,
            sessions,
            current,
            selected_index,
        };
        let picker = cx.new(|cx| Picker::nonsearchable_uniform_list(delegate, window, cx));
        Self { picker }
    }
}

impl Render for AgentSessionPicker {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        v_flex().w(rems(34.)).child(self.picker.clone())
    }
}

impl Focusable for AgentSessionPicker {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.picker.focus_handle(cx)
    }
}

impl EventEmitter<DismissEvent> for AgentSessionPicker {}
impl ModalView for AgentSessionPicker {}

struct AgentSessionPickerDelegate {
    picker: WeakEntity<AgentSessionPicker>,
    terminal_view: WeakEntity<TerminalView>,
    sessions: Vec<(SessionLocation, Option<String>)>,
    current: Option<std::path::PathBuf>,
    selected_index: usize,
}

/// How long ago `time` was, briefly.
fn time_ago(time: Option<SystemTime>) -> String {
    let Some(elapsed) = time.and_then(|time| time.elapsed().ok()) else {
        return String::new();
    };
    let minutes = elapsed.as_secs() / 60;
    let chinese = util::i18n::is_chinese();
    match (minutes, chinese) {
        (0, false) => "just now".to_string(),
        (0, true) => "刚刚".to_string(),
        (1..60, false) => format!("{minutes} min ago"),
        (1..60, true) => format!("{minutes} 分钟前"),
        (60..1440, false) => format!("{} h ago", minutes / 60),
        (60..1440, true) => format!("{} 小时前", minutes / 60),
        (_, false) => format!("{} d ago", minutes / 1440),
        (_, true) => format!("{} 天前", minutes / 1440),
    }
}

impl PickerDelegate for AgentSessionPickerDelegate {
    type ListItem = ListItem;

    fn name() -> &'static str {
        "agent session picker"
    }

    fn placeholder_text(&self, _window: &mut Window, _cx: &mut App) -> Arc<str> {
        util::i18n::tr(
            "Choose the agent session this terminal runs…",
            "选择这个终端运行的 agent 会话…",
        )
        .into()
    }

    fn match_count(&self) -> usize {
        self.sessions.len()
    }

    fn confirm(&mut self, _: bool, window: &mut Window, cx: &mut Context<Picker<Self>>) {
        if let Some((location, _)) = self.sessions.get(self.selected_index) {
            let location = location.clone();
            self.terminal_view
                .update(cx, |terminal_view, cx| {
                    terminal_view.agent_session_chosen(location, cx)
                })
                .ok();
        }
        self.dismissed(window, cx);
    }

    fn dismissed(&mut self, _: &mut Window, cx: &mut Context<Picker<Self>>) {
        self.picker.update(cx, |_, cx| cx.emit(DismissEvent)).ok();
    }

    fn selected_index(&self) -> usize {
        self.selected_index
    }

    fn set_selected_index(&mut self, ix: usize, _: &mut Window, _: &mut Context<Picker<Self>>) {
        self.selected_index = ix;
    }

    fn update_matches(
        &mut self,
        _query: String,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) -> Task<()> {
        Task::ready(())
    }

    fn render_match(
        &self,
        ix: usize,
        selected: bool,
        _: &mut Window,
        _: &mut Context<Picker<Self>>,
    ) -> Option<Self::ListItem> {
        let (location, description) = self.sessions.get(ix)?;
        let label = description
            .clone()
            .unwrap_or_else(|| location.session_id.clone());
        let mut item = ListItem::new(ix)
            .inset(true)
            .spacing(ListItemSpacing::Sparse)
            .toggle_state(selected)
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .justify_between()
                    .child(Label::new(label).truncate())
                    .child(
                        Label::new(time_ago(location.modified))
                            .color(Color::Muted)
                            .size(LabelSize::Small),
                    ),
            );
        if self.current.as_ref() == Some(&location.path) {
            item = item.end_slot(Icon::new(IconName::Check).color(Color::Muted));
        }
        Some(item)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage() -> agent_sessions::UsageSummary {
        use agent_sessions::{TokenUsage, UsageSummary};
        UsageSummary {
            session: TokenUsage {
                input: 1_000,
                output: 30_000,
                cache_read: 211_400_000,
                cache_creation: 0,
            },
            turn: TokenUsage {
                input: 500,
                output: 482,
                cache_read: 541_500,
                cache_creation: 0,
            },
            context_tokens: Some(850_000),
            model: Some("claude-opus-5-5".into()),
            context_window: Some(1_000_000),
        }
    }

    #[test]
    fn test_usage_bar_contents() {
        // Nothing known: no bar.
        assert_eq!(UsageBar::new(&Default::default()), None);
        assert_eq!(
            UsageBar::new(&usage()),
            Some(UsageBar {
                context: Some(("850k / 1M".to_string(), Some(0.85))),
                model: Some("opus-5-5".to_string()),
                turn: Some(("542k".to_string(), "482".to_string())),
                session: Some(("211M".to_string(), "30k".to_string())),
            })
        );
    }

    #[gpui::test]
    async fn test_usage_bar_drops_whole_parts(cx: &mut gpui::TestAppContext) {
        use gpui::Bounds;
        struct Bar {
            width: Pixels,
        }
        impl Render for Bar {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                let contents = UsageBar::new(&usage()).unwrap();
                div().w(self.width).child(usage_bar(&contents, cx))
            }
        }
        crate::tests::init_test(cx).await;
        let (bar, cx) = cx.add_window_view(|_, _| Bar { width: px(2000.) });
        // Which parts show at `width`: those on the first line.
        let mut shown_at = |width: Pixels| {
            bar.update(cx, |bar, cx| {
                bar.width = width;
                cx.notify();
            });
            cx.run_until_parked();
            let line = cx.debug_bounds("agent-usage-parts").expect("the bar");
            [
                "agent-usage-context",
                "agent-usage-model",
                "agent-usage-turn",
                "agent-usage-session",
            ]
            .into_iter()
            .map(|part| {
                let bounds = cx.debug_bounds(part).expect("a part");
                (bounds.top() < line.bottom(), bounds)
            })
            .collect::<Vec<_>>()
        };
        let shown = |parts: &[(bool, Bounds<Pixels>)]| {
            parts.iter().map(|(shown, _)| *shown).collect::<Vec<_>>()
        };
        let wide = shown_at(px(2000.));
        assert_eq!(shown(&wide), [true, true, true, true]);
        let padding = wide[0].1.left();
        let end_of = |ix: usize| wide[ix].1.right() + padding;

        // One pixel short of the session's tokens: they go, the rest stays.
        assert_eq!(
            shown(&shown_at(end_of(3) - px(1.))),
            [true, true, true, false]
        );
        // Then the turn's, then the model.
        assert_eq!(
            shown(&shown_at(end_of(2) - px(1.))),
            [true, true, false, false]
        );
        assert_eq!(
            shown(&shown_at(end_of(1) - px(1.))),
            [true, false, false, false]
        );
        // Then the context with its label and tokens goes too, and only its
        // bar and percentage, drawn below, show.
        let narrow = shown_at(end_of(0) - px(1.));
        assert_eq!(shown(&narrow), [false, false, false, false]);
        let minimal = cx
            .debug_bounds("agent-usage-minimal")
            .expect("the short context");
        assert!(minimal.right() <= end_of(0) - px(1.), "{minimal:?}");
    }
}
