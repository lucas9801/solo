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
    Subscription, Task, WeakEntity, Window, actions,
};
use picker::{Picker, PickerDelegate};
use terminal::shell_integration::AgentSessionId;
use ui::{ListItem, ListItemSpacing, prelude::*};
use workspace::ModalView;

use crate::TerminalView;

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
    /// Whether the user was told the context is nearly full.
    warned_context_full: bool,
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
    /// context and token usage. When narrow, it's cut from the end: the
    /// turn's tokens first, then the session's.
    pub(crate) fn render_agent_usage_bar(&self, cx: &App) -> Option<AnyElement> {
        let usage = self.shown_agent_usage(cx)?;
        let parts = usage_bar_parts(&usage, util::i18n::is_chinese());
        if parts.is_empty() {
            return None;
        }
        Some(
            h_flex()
                .id("agent-usage-bar")
                .w_full()
                .px_2()
                .gap_3()
                .overflow_hidden()
                .border_t_1()
                .border_color(cx.theme().colors().border_variant)
                .bg(cx.theme().colors().terminal_background)
                .children(parts.into_iter().map(|(text, nearly_full)| {
                    div().flex_none().child(
                        Label::new(text)
                            .untranslated()
                            .single_line()
                            .size(LabelSize::XSmall)
                            .color(if nearly_full {
                                Color::Warning
                            } else {
                                Color::Muted
                            }),
                    )
                }))
                .into_any_element(),
        )
    }

    /// SOLO: how full the running agent's context is, for its tab: the
    /// percentage, and whether it's nearly full.
    pub(crate) fn agent_context_percent(&self, cx: &App) -> Option<(u32, bool)> {
        let usage = self.shown_agent_usage(cx)?;
        let fraction = usage.context_fraction()?;
        Some((
            (fraction * 100.).round() as u32,
            usage.context_nearly_full(),
        ))
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
        self.agent_session.warned_context_full = false;
        session
    }

    fn agent_session_event(
        &mut self,
        session: Entity<AgentSession>,
        _: &AgentSessionEvent,
        cx: &mut Context<Self>,
    ) {
        let nearly_full = session.read(cx).state().usage.context_nearly_full();
        if nearly_full && !self.agent_session.warned_context_full {
            self.agent_session.warned_context_full = true;
            let state = session.read(cx).state();
            let (agent, usage) = (state.agent(), state.usage.clone());
            self.warn_context_nearly_full(agent, &usage, cx);
        } else if !nearly_full {
            self.agent_session.warned_context_full = false;
        }
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

    fn warn_context_nearly_full(
        &self,
        agent_kind: agent_sessions::AgentKind,
        usage: &agent_sessions::UsageSummary,
        cx: &mut Context<Self>,
    ) {
        let Some(percent) = usage
            .context_fraction()
            .map(|fraction| (fraction * 100.).round())
        else {
            return;
        };
        let agent = agent_kind.name();
        let model = usage.model.clone().unwrap_or_default();
        let compact_hint = match agent_kind {
            agent_sessions::AgentKind::ClaudeCode | agent_sessions::AgentKind::Codex => {
                " (/compact)"
            }
        };
        let message = if util::i18n::is_chinese() {
            format!("{agent} 的上下文已用 {percent}%（{model}），建议压缩{compact_hint}或开新会话")
        } else {
            format!(
                "{agent}'s context is {percent}% full ({model}). Consider compacting it{compact_hint} or starting a new session."
            )
        };
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let id = workspace::notifications::NotificationId::composite::<ChooseAgentSession>((
            "agent-context-full",
            cx.entity_id(),
        ));
        workspace.update(cx, |workspace, cx| {
            workspace.show_toast(workspace::Toast::new(id, message), cx)
        });
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

/// A token count, briefly: 950, 12.3k, 4.5M.
fn format_tokens(tokens: u64) -> String {
    match tokens {
        0..1_000 => tokens.to_string(),
        1_000..1_000_000 => format!("{:.1}k", tokens as f64 / 1_000.),
        _ => format!("{:.1}M", tokens as f64 / 1_000_000.),
    }
}

/// SOLO: the parts of an agent's usage bar, most important first, each with
/// whether to warn: the context (and model), the session's tokens, the
/// turn's tokens. Cache details stay in the tooltip.
pub(crate) fn usage_bar_parts(
    usage: &agent_sessions::UsageSummary,
    chinese: bool,
) -> Vec<(String, bool)> {
    let mut parts = Vec::new();
    if let Some(context_tokens) = usage.context_tokens {
        let context = match (usage.context_window, usage.context_fraction()) {
            (Some(window), Some(fraction)) => format!(
                "{} / {} ({:.0}%)",
                format_tokens(context_tokens),
                format_tokens(window),
                fraction * 100.
            ),
            _ => format_tokens(context_tokens),
        };
        let mut text = if chinese {
            format!("上下文 {context}")
        } else {
            format!("Context {context}")
        };
        if let Some(model) = usage.model.as_deref().filter(|model| !model.is_empty()) {
            text.push_str(&format!(" · {model}"));
        }
        parts.push((text, usage.context_nearly_full()));
    }
    if !usage.session.is_empty() {
        let session = &usage.session;
        parts.push((
            if chinese {
                format!(
                    "本会话 输入 {} · 输出 {}",
                    format_tokens(session.total_input()),
                    format_tokens(session.output)
                )
            } else {
                format!(
                    "Session {} in · {} out",
                    format_tokens(session.total_input()),
                    format_tokens(session.output)
                )
            },
            false,
        ));
        let turn = &usage.turn;
        parts.push((
            if chinese {
                format!(
                    "本轮 输入 {} · 输出 {}",
                    format_tokens(turn.total_input()),
                    format_tokens(turn.output)
                )
            } else {
                format!(
                    "Turn {} in · {} out",
                    format_tokens(turn.total_input()),
                    format_tokens(turn.output)
                )
            },
            false,
        ));
    }
    parts
}

/// The lines about a session in its terminal's tab tooltip: its title, token
/// usage and how full its context is. Costs aren't shown: the only one known
/// is the agent's own estimate at API prices, written now and then.
pub(crate) fn session_tooltip_lines(
    state: &SessionState,
    matched_by: MatchedBy,
    chinese: bool,
) -> Vec<String> {
    let mut lines = Vec::new();
    let guessed = matches!(matched_by, MatchedBy::Recent { ambiguous: true });
    if let Some(title) = &state.title {
        lines.push(match (chinese, guessed) {
            (false, false) => format!("Session: {title}"),
            (false, true) => format!("Session (guessed): {title}"),
            (true, false) => format!("会话：{title}"),
            (true, true) => format!("会话（推测）：{title}"),
        });
    }
    let usage = &state.usage;
    if !usage.session.is_empty() {
        let session = &usage.session;
        lines.push(if chinese {
            format!(
                "本会话 token：输入 {} · 输出 {}（缓存读 {} · 缓存写 {} · 未缓存 {}）",
                format_tokens(session.total_input()),
                format_tokens(session.output),
                format_tokens(session.cache_read),
                format_tokens(session.cache_creation),
                format_tokens(session.input),
            )
        } else {
            format!(
                "Session tokens: {} in · {} out ({} cache read · {} cache write · {} uncached)",
                format_tokens(session.total_input()),
                format_tokens(session.output),
                format_tokens(session.cache_read),
                format_tokens(session.cache_creation),
                format_tokens(session.input),
            )
        });
        let turn = &usage.turn;
        lines.push(if chinese {
            format!(
                "本轮 token：输入 {} · 输出 {}",
                format_tokens(turn.total_input()),
                format_tokens(turn.output)
            )
        } else {
            format!(
                "Turn tokens: {} in · {} out",
                format_tokens(turn.total_input()),
                format_tokens(turn.output)
            )
        });
    }
    if let Some(context_tokens) = usage.context_tokens {
        let model = usage.model.as_deref().unwrap_or_default();
        let context = match (usage.context_window, usage.context_fraction()) {
            (Some(window), Some(fraction)) => format!(
                "{} / {} ({:.0}%)",
                format_tokens(context_tokens),
                format_tokens(window),
                fraction * 100.
            ),
            _ => format_tokens(context_tokens),
        };
        let mut line = if chinese {
            format!("上下文：{context}  {model}")
        } else {
            format!("Context: {context}  {model}")
        };
        if usage.context_nearly_full() {
            line.push_str(util::i18n::tr(
                " — nearly full, compact it or start a new session",
                " — 快满了，建议压缩或开新会话",
            ));
        }
        lines.push(line.trim_end().to_string());
    }
    lines
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
    use agent_sessions::{AgentKind, ClaudeCode};

    #[test]
    fn test_format_tokens() {
        assert_eq!(format_tokens(950), "950");
        assert_eq!(format_tokens(12_345), "12.3k");
        assert_eq!(format_tokens(4_520_000), "4.5M");
    }

    #[test]
    fn test_usage_bar_parts() {
        use agent_sessions::{TokenUsage, UsageSummary};
        // Nothing known: no bar.
        assert!(usage_bar_parts(&UsageSummary::default(), false).is_empty());
        let usage = UsageSummary {
            session: TokenUsage {
                input: 1_000,
                output: 30_000,
                cache_read: 1_200_000,
                cache_creation: 0,
            },
            turn: TokenUsage {
                input: 500,
                output: 2_000,
                cache_read: 50_000,
                cache_creation: 0,
            },
            context_tokens: Some(170_000),
            model: Some("claude-opus".into()),
            context_window: Some(200_000),
        };
        // Context first (cut last when narrow), warned of past 80%.
        assert_eq!(
            usage_bar_parts(&usage, false),
            vec![
                (
                    "Context 170.0k / 200.0k (85%) · claude-opus".to_string(),
                    true
                ),
                ("Session 1.2M in · 30.0k out".to_string(), false),
                ("Turn 50.5k in · 2.0k out".to_string(), false),
            ]
        );
        assert_eq!(
            usage_bar_parts(&usage, true)[1].0,
            "本会话 输入 1.2M · 输出 30.0k"
        );
    }

    #[test]
    fn test_session_tooltip_lines() {
        let mut state = SessionState::new(AgentKind::ClaudeCode);
        assert!(session_tooltip_lines(&state, MatchedBy::Reported, false).is_empty());
        for line in [
            r#"{"type":"user","message":{"content":"Go"}}"#,
            r#"{"type":"assistant","message":{"model":"claude-haiku-4-5","id":"m1","content":[],"usage":{"input_tokens":10,"output_tokens":2000,"cache_read_input_tokens":150000,"cache_creation_input_tokens":20000}}}"#,
            r#"{"type":"ai-title","aiTitle":"Fix the build"}"#,
        ] {
            state.apply_all(ClaudeCode.parse_line(line));
        }
        assert_eq!(
            session_tooltip_lines(&state, MatchedBy::Recent { ambiguous: true }, false),
            [
                "Session (guessed): Fix the build",
                "Session tokens: 170.0k in · 2.0k out (150.0k cache read · 20.0k cache write · 10 uncached)",
                "Turn tokens: 170.0k in · 2.0k out",
                &format!(
                    "Context: 170.0k / 200.0k (85%)  claude-haiku-4-5{}",
                    util::i18n::tr(
                        " — nearly full, compact it or start a new session",
                        " — 快满了，建议压缩或开新会话",
                    )
                ),
            ]
        );
        assert_eq!(
            session_tooltip_lines(&state, MatchedBy::Reported, true)[2],
            "本轮 token：输入 170.0k · 输出 2.0k"
        );
    }
}
