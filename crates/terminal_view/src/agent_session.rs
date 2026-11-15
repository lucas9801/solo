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
    AgentAdapter, AgentKind, AgentSession, AgentSessionEvent, MatchedBy, ResponseSummary,
    ResponseTiming, SessionLocation, SessionState, TerminalProbe, TranscriptReader,
};
use gpui::{
    App, AppContext as _, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable,
    Stateful, Subscription, Task, WeakEntity, Window, actions,
};
use picker::{Picker, PickerDelegate};
use terminal::shell_integration::{AgentResponse, AgentSessionId};
use ui::{ListItem, ListItemSpacing, prelude::*};
use workspace::ModalView;

use crate::{
    TerminalView,
    agent_tab::{
        context_color, context_hsla, format_first_token, format_speed, format_tokens_short,
        format_work_time, render_context_bar, short_model,
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
/// How often Copilot CLI's telemetry file is read.
const COPILOT_TELEMETRY_POLL: Duration = Duration::from_secs(1);
/// A session whose first prompt came this long before the agent's run
/// started was resumed: its earlier turns' time isn't known.
const RESUMED_SLACK: Duration = Duration::from_secs(5);

/// SOLO: what's shown of how the agent works, besides its tokens: its
/// reasoning effort, how fast its model answers, and how long it worked.
/// Each is `None` when it can't be told for sure.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct AgentMetrics {
    pub effort: Option<String>,
    pub responses: ResponseSummary,
    /// The turn under way, or else the last one.
    pub turn_time: Option<Duration>,
    /// All the session's turns.
    pub session_time: Option<Duration>,
}

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

    /// SOLO: whether the running agent's usage is to be shown outside the
    /// tooltip (`agent_usage_display`).
    fn shows_agent_usage(&self, cx: &App) -> bool {
        use settings::Settings as _;
        let display =
            terminal::terminal_settings::TerminalSettings::get_global(cx).agent_usage_display;
        display == terminal::terminal_settings::AgentUsageDisplay::Always
            && self
                .agent_status()
                .is_some_and(|(_, status)| status.is_running())
    }

    /// SOLO: the usage of the agent running in this terminal, when it's to
    /// be shown outside the tooltip (`agent_usage_display`).
    fn shown_agent_usage(&self, cx: &App) -> Option<agent_sessions::UsageSummary> {
        if !self.shows_agent_usage(cx) {
            return None;
        }
        Some(self.agent_session()?.read(cx).state().usage.clone())
    }

    /// SOLO: the thin bar at the bottom of an agent's terminal with its
    /// context and token usage, how fast its model answers and how long it
    /// worked (see [`usage_bar`]).
    pub(crate) fn render_agent_usage_bar(&self, cx: &App) -> Option<AnyElement> {
        if !self.shows_agent_usage(cx) {
            return None;
        }
        let session = self.agent_session().map(|session| session.read(cx));
        let usage = session.map(|session| &session.state().usage);
        let contents = UsageBar::new(usage, &self.agent_metrics(cx))?;
        Some(usage_bar(&contents, cx).into_any_element())
    }

    /// SOLO: the agent's effort, how fast its model answers and how long it
    /// worked, from the surest source for the agent: its own records (Claude
    /// Code's transcript, Codex's telemetry, Codex's rollout for the effort),
    /// else what it reported (OpenCode, through Solo's plugin) or Copilot
    /// CLI's telemetry file. The times are from its status.
    pub(crate) fn agent_metrics(&self, cx: &App) -> AgentMetrics {
        let now = Instant::now();
        let activity = &self.agent_activity;
        let session = self.agent_session().map(|session| session.read(cx));
        let state = session.map(|session| session.state());
        let mut responses = state
            .map(|state| state.responses.summary())
            .filter(|summary| !summary.is_empty());
        let mut effort = state.and_then(|state| state.effort.clone());
        if let Some(session) = session
            && session.location().agent == AgentKind::Codex
            && let Some(receiver) = agent_sessions::codex_otel::running_receiver()
        {
            let conversation = &session.location().session_id;
            responses = responses.or_else(|| receiver.summary(conversation));
            effort = effort.or_else(|| receiver.effort(conversation));
        }
        let responses = responses.unwrap_or_else(|| activity.responses.summary());
        let effort = effort.or_else(|| activity.reported_effort.clone());
        // A resumed session's earlier turns weren't timed: no total then.
        let resumed = state
            .and_then(|state| state.first_prompt_at_ms())
            .map(|first_prompt| {
                let run_started = self
                    .agent_run_started_at()
                    .and_then(|started_at| SystemTime::now().checked_sub(started_at.elapsed()))
                    .and_then(|started_at| started_at.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|since_epoch| since_epoch.as_millis() as i64);
                run_started.is_none_or(|run_started| {
                    first_prompt < run_started - RESUMED_SLACK.as_millis() as i64
                })
            });
        AgentMetrics {
            effort,
            responses,
            turn_time: activity.clock.turn(now),
            session_time: activity
                .clock
                .session(now)
                .filter(|_| resumed != Some(true)),
        }
    }

    /// SOLO: the agent (OpenCode, through Solo's plugin) reported how fast
    /// its model answered a request.
    pub(crate) fn agent_response_reported(
        &mut self,
        response: &AgentResponse,
        cx: &mut Context<Self>,
    ) {
        let activity = &mut self.agent_activity;
        let timing = ResponseTiming {
            first_token: response.first_token_ms.map(Duration::from_millis),
            output_tokens: response.output_tokens,
            generation: response.generation_ms.map(Duration::from_millis),
        };
        if timing.first_token.is_some() || timing.generation.is_some() {
            let key = activity.responses.len().to_string();
            activity.responses.record(&key, timing, false);
        }
        if let Some(effort) = &response.effort {
            activity.reported_effort = Some(effort.clone());
        }
        cx.notify();
    }

    /// SOLO: while Copilot CLI runs, reads what it appends to the telemetry
    /// file Solo gave this terminal (see
    /// `terminal::shell_integration::integrate_copilot`).
    pub(crate) fn keep_copilot_telemetry(&mut self, cx: &mut Context<Self>) {
        if self.agent_activity.copilot_telemetry.is_some()
            || self
                .agent_status()
                .is_none_or(|(name, _)| !name.eq_ignore_ascii_case("copilot"))
        {
            return;
        }
        let Some(path) = self
            .terminal
            .read(cx)
            .shell_integration()
            .terminal_id()
            .and_then(terminal::shell_integration::copilot_telemetry_file)
        else {
            return;
        };
        let task = cx.spawn(async move |this, cx| {
            let mut reader = TranscriptReader::from_end(path);
            let mut spans = 0usize;
            loop {
                cx.background_executor().timer(COPILOT_TELEMETRY_POLL).await;
                let (returned, timings) = cx
                    .background_spawn(async move {
                        let timings = reader
                            .read_new_lines()
                            .map(|new_lines| {
                                new_lines
                                    .lines
                                    .iter()
                                    .flat_map(|line| agent_sessions::copilot_otel::parse_line(line))
                                    .collect::<Vec<_>>()
                            })
                            .unwrap_or_default();
                        (reader, timings)
                    })
                    .await;
                reader = returned;
                let updated = this.update(cx, |this, cx| {
                    if timings.is_empty() {
                        return;
                    }
                    for (id, timing) in timings {
                        spans += 1;
                        let key = id.unwrap_or_else(|| format!("span-{spans}"));
                        this.agent_activity.responses.record(&key, timing, false);
                    }
                    cx.notify();
                });
                if updated.is_err() {
                    break;
                }
            }
        });
        self.agent_activity.copilot_telemetry = Some(task);
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
        // SOLO: another session in the same run (`/clear`, say) counts its
        // time afresh.
        if let Some(previous) = &self.agent_session.session
            && previous.read(cx).location().session_id != location.session_id
        {
            self.agent_activity.clock.restart_session();
        }
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

    pub(crate) fn agent_session_chosen(
        &mut self,
        location: SessionLocation,
        cx: &mut Context<Self>,
    ) {
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
    /// The model and its reasoning effort: "opus-5-5 · xhigh".
    model: Option<String>,
    /// The latest request's first token and speed: "5.1 s", "128 token/s".
    response: Option<(Option<String>, Option<String>)>,
    /// How long the turn has taken: "2:14".
    turn_time: Option<String>,
    /// The turn's and the session's tokens, in and out.
    turn: Option<(String, String)>,
    session: Option<(String, String)>,
}

impl UsageBar {
    fn new(usage: Option<&agent_sessions::UsageSummary>, metrics: &AgentMetrics) -> Option<Self> {
        let default_usage = agent_sessions::UsageSummary::default();
        let usage = usage.unwrap_or(&default_usage);
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
        let model = usage
            .model
            .as_deref()
            .filter(|model| !model.is_empty())
            .map(|model| short_model(model).to_string());
        let model = match (model, metrics.effort.clone()) {
            (Some(model), Some(effort)) => Some(format!("{model} · {effort}")),
            (model, effort) => model.or(effort),
        };
        let first_token = metrics.responses.latest_first_token.map(format_first_token);
        let speed = metrics.responses.latest_speed.map(format_speed);
        let bar = Self {
            context,
            model,
            response: (first_token.is_some() || speed.is_some()).then_some((first_token, speed)),
            turn_time: metrics.turn_time.map(format_work_time),
            turn,
            session,
        };
        (bar.context.is_some()
            || bar.model.is_some()
            || bar.session.is_some()
            || bar.response.is_some()
            || bar.turn_time.is_some())
        .then_some(bar)
    }
}

/// SOLO: the usage bar: the context (a short bar, its percentage and
/// tokens), the model and its effort, the latest request's first token and
/// speed, the turn's time and tokens and the session's tokens. When narrow,
/// whole parts go: the session's tokens first, then the turn's, then the
/// first token and speed, then the model; last, only the context's bar and
/// percentage stay.
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
    let separated = |name: &'static str| {
        part(name).ml_3().child(
            div()
                .flex_none()
                .w(px(1.))
                .h(rems_from_px(14_f32))
                .mr_1()
                .bg(cx.theme().colors().border),
        )
    };
    let tokens =
        |name: &'static str, what: &str, time: Option<String>, tokens: Option<(String, String)>| {
            let has_time = time.is_some();
            separated(name)
                .child(muted(what))
                .when_some(time, |this, time| this.child(number(time)))
                .when_some(tokens, |this, (input, output)| {
                    this.when(has_time, |this| this.child(muted("·")))
                        .child(muted(util::i18n::tr("in", "输入")))
                        .child(number(input))
                        .child(muted(util::i18n::tr("out", "输出")))
                        .child(number(output))
                })
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
    if let Some((first_token, speed)) = contents.response.clone() {
        let both = first_token.is_some() && speed.is_some();
        parts = parts.child(
            separated("response")
                .when_some(first_token, |this, first_token| {
                    this.child(muted(util::i18n::tr("First token", "首字")))
                        .child(number(first_token))
                })
                .when(both, |this| this.child(muted("·")))
                .when_some(speed, |this, speed| this.child(number(speed))),
        );
    }
    if contents.turn.is_some() || contents.turn_time.is_some() {
        parts = parts.child(tokens(
            "turn",
            util::i18n::tr("Turn", "本轮"),
            contents.turn_time.clone(),
            contents.turn.clone(),
        ));
    }
    if let Some(session) = contents.session.clone() {
        parts = parts.child(tokens(
            "session",
            util::i18n::tr("Session", "本会话"),
            None,
            Some(session),
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

    fn metrics() -> AgentMetrics {
        AgentMetrics {
            effort: Some("xhigh".to_string()),
            responses: ResponseSummary {
                latest_first_token: Some(Duration::from_millis(5_100)),
                latest_speed: Some(128.),
                mean_first_token: Some(Duration::from_millis(5_300)),
                mean_speed: Some(96.),
            },
            turn_time: Some(Duration::from_secs(134)),
            session_time: Some(Duration::from_secs(4325)),
        }
    }

    #[test]
    fn test_usage_bar_contents() {
        // Nothing known: no bar.
        assert_eq!(UsageBar::new(None, &AgentMetrics::default()), None);
        assert_eq!(
            UsageBar::new(Some(&Default::default()), &AgentMetrics::default()),
            None
        );
        assert_eq!(
            UsageBar::new(Some(&usage()), &AgentMetrics::default()),
            Some(UsageBar {
                context: Some(("850k / 1M".to_string(), Some(0.85))),
                model: Some("opus-5-5".to_string()),
                response: None,
                turn_time: None,
                turn: Some(("542k".to_string(), "482".to_string())),
                session: Some(("211M".to_string(), "30k".to_string())),
            })
        );
        assert_eq!(
            UsageBar::new(Some(&usage()), &metrics()),
            Some(UsageBar {
                context: Some(("850k / 1M".to_string(), Some(0.85))),
                model: Some("opus-5-5 · xhigh".to_string()),
                response: Some((Some("5.1 s".to_string()), Some("128 token/s".to_string()))),
                turn_time: Some("2:14".to_string()),
                turn: Some(("542k".to_string(), "482".to_string())),
                session: Some(("211M".to_string(), "30k".to_string())),
            })
        );
        // An agent whose screen only is read: its turn's time alone.
        let screen_only = AgentMetrics {
            turn_time: Some(Duration::from_secs(42)),
            ..Default::default()
        };
        assert_eq!(
            UsageBar::new(None, &screen_only),
            Some(UsageBar {
                context: None,
                model: None,
                response: None,
                turn_time: Some("0:42".to_string()),
                turn: None,
                session: None,
            })
        );
        // Only what's known of the response.
        let speed_only = AgentMetrics {
            responses: ResponseSummary {
                latest_speed: Some(64.4),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(
            UsageBar::new(None, &speed_only).unwrap().response,
            Some((None, Some("64 token/s".to_string())))
        );
    }

    /// The times come from the agent's status and the speeds from what it
    /// reported.
    #[gpui::test]
    async fn test_agent_metrics(cx: &mut gpui::TestAppContext) {
        use gpui::AppContext as _;
        let (project, workspace) = crate::tests::init_test(cx).await;
        let terminal = cx.new(|cx| {
            terminal::TerminalBuilder::new_display_only(
                Default::default(),
                settings::AlternateScroll::On,
                None,
                0,
                cx.background_executor(),
                util::paths::PathStyle::local(),
            )
            .subscribe(cx)
        });
        let (view, cx) = cx.add_window_view(|window, cx| {
            TerminalView::new(
                terminal.clone(),
                workspace.downgrade(),
                None,
                project.downgrade(),
                window,
                cx,
            )
        });
        view.update(cx, |view, cx| {
            assert_eq!(view.agent_metrics(cx), AgentMetrics::default());
            let start = Instant::now() - Duration::from_secs(10);
            view.agent_activity.clock.update(
                Some(crate::agent_status::AgentStatus::Working),
                start,
                start,
            );
            view.agent_response_reported(
                &AgentResponse {
                    raw_session_id: None,
                    first_token_ms: Some(1500),
                    output_tokens: Some(2000),
                    generation_ms: Some(10_000),
                    effort: Some("high".to_string()),
                },
                cx,
            );
            // Nothing measured: not counted.
            view.agent_response_reported(
                &AgentResponse {
                    output_tokens: Some(10),
                    ..Default::default()
                },
                cx,
            );
            let metrics = view.agent_metrics(cx);
            assert_eq!(metrics.effort.as_deref(), Some("high"));
            assert_eq!(
                metrics.responses.latest_first_token,
                Some(Duration::from_millis(1500))
            );
            assert_eq!(metrics.responses.latest_speed, Some(200.));
            assert_eq!(metrics.responses.mean_speed, Some(200.));
            assert!(metrics.turn_time.unwrap() >= Duration::from_secs(10));
            assert!(metrics.session_time.unwrap() >= Duration::from_secs(10));
        });
    }

    #[gpui::test]
    async fn test_usage_bar_drops_whole_parts(cx: &mut gpui::TestAppContext) {
        use gpui::Bounds;
        struct Bar {
            width: Pixels,
        }
        impl Render for Bar {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                let contents = UsageBar::new(Some(&usage()), &metrics()).unwrap();
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
                "agent-usage-response",
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
        assert_eq!(shown(&wide), [true, true, true, true, true]);
        let padding = wide[0].1.left();
        let end_of = |ix: usize| wide[ix].1.right() + padding;

        // One pixel short of the session's tokens: they go, the rest stays.
        assert_eq!(
            shown(&shown_at(end_of(4) - px(1.))),
            [true, true, true, true, false]
        );
        // Then the turn's, then the first token and speed, then the model.
        assert_eq!(
            shown(&shown_at(end_of(3) - px(1.))),
            [true, true, true, false, false]
        );
        assert_eq!(
            shown(&shown_at(end_of(2) - px(1.))),
            [true, true, false, false, false]
        );
        assert_eq!(
            shown(&shown_at(end_of(1) - px(1.))),
            [true, false, false, false, false]
        );
        // Then the context with its label and tokens goes too, and only its
        // bar and percentage, drawn below, show.
        let narrow = shown_at(end_of(0) - px(1.));
        assert_eq!(shown(&narrow), [false, false, false, false, false]);
        let minimal = cx
            .debug_bounds("agent-usage-minimal")
            .expect("the short context");
        assert!(minimal.right() <= end_of(0) - px(1.), "{minimal:?}");
    }
}
