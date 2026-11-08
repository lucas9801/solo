//! SOLO: what an AI agent running in a terminal (Claude Code, Codex, ...) is
//! doing, shown on its tab: working, waiting for you, idle, done or failed.
//!
//! Claude Code started from Solo's shell integration reports it through hooks
//! (see `terminal::shell_integration::integrate_claude_code`): a submitted
//! prompt starts a turn, a permission request or question waits for the user,
//! and stopping ends the turn. Once an agent has reported, its reports decide.
//! Other agents and scripts can report the same way
//! (docs/agent-status-protocol.md), which also makes a program Solo doesn't
//! know an agent.
//!
//! A turn that ends while you aren't looking at its terminal shows as done
//! until you do; then the agent is idle.
//!
//! Other agents don't report it, so it's read from the screen, with rules for
//! each agent (`crate::agent_detection`): its spinner, its prompt, its
//! questions. A question plainly on screen also wins over a hook's report.
//! Agents without rules are judged by their output: they animate a spinner
//! while they work and go quiet when they stop, and ring the bell or ask for
//! a notification when they need the user. Output right after input (echoed
//! keys, redraws after a resize or a focus change) doesn't count as working.
//! Read from the screen, working starts a turn and stopping ends it, as hooks
//! do.

use std::time::{Duration, Instant};

use gpui::{Context, EventEmitter, Task, Window};
use settings::Settings;
use terminal::shell_integration::{AgentHook, AgentHookKind};
use terminal::terminal_settings::TerminalSettings;
use ui::{Color, IconName};
use workspace::item::ItemEvent;

use crate::agent_detection::{self, ScreenReading};
use crate::{TerminalView, command_blocks::is_agent_command};

/// How often the screen is read while output keeps coming.
const SCREEN_READ_INTERVAL: Duration = Duration::from_millis(150);
/// After output, when the screen is read again if nothing else happens.
const SCREEN_SETTLE: Duration = Duration::from_millis(300);

/// SOLO: An agent that reports through hooks started or ended a turn: from the
/// prompt the user submitted to when it stopped, was interrupted or exited.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentTurnEvent {
    Started,
    Ended,
}

impl EventEmitter<AgentTurnEvent> for TerminalView {}

/// Output this soon after input is taken as the program answering it.
const ECHO_WINDOW: Duration = Duration::from_millis(400);
/// An agent that printed nothing of its own for this long has stopped working.
const QUIET_AFTER: Duration = Duration::from_secs(3);
/// The working icon turns in steps: animating it every display frame would
/// redraw its tab bar, and the terminal shown below it, 60 to 144 times a
/// second for as long as an agent works.
const SPINNER_STEP: Duration = Duration::from_millis(125);
const SPINNER_STEPS_PER_TURN: u128 = 16;
/// Notify when an agent stops after working at least this long in the background.
pub(crate) const NOTIFY_AFTER_WORKING: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentStatus {
    Working,
    /// The agent asked for permission or an answer (or, without hooks, rang
    /// the bell or asked for a notification), and nothing has happened since.
    NeedsInput,
    /// Running but quiet, likely waiting for the next prompt.
    Idle,
    /// Still running, and finished a turn you haven't looked at yet.
    Finished,
    /// Exited.
    Done,
    Failed,
}

impl AgentStatus {
    /// Whether the agent is still running.
    pub(crate) fn is_running(self) -> bool {
        matches!(
            self,
            AgentStatus::Working
                | AgentStatus::NeedsInput
                | AgentStatus::Idle
                | AgentStatus::Finished
        )
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            AgentStatus::Working => util::i18n::tr("Working", "工作中"),
            AgentStatus::NeedsInput => util::i18n::tr("Needs your input", "需要你的输入"),
            AgentStatus::Idle => util::i18n::tr("Idle", "空闲"),
            AgentStatus::Finished | AgentStatus::Done => util::i18n::tr("Done", "已完成"),
            AgentStatus::Failed => util::i18n::tr("Failed", "失败"),
        }
    }

    pub(crate) fn icon(self) -> (IconName, Color) {
        match self {
            AgentStatus::Working => (IconName::LoadCircle, Color::Accent),
            AgentStatus::NeedsInput => (IconName::Bell, Color::Warning),
            AgentStatus::Idle => (IconName::Circle, Color::Muted),
            AgentStatus::Finished | AgentStatus::Done => (IconName::Check, Color::Success),
            AgentStatus::Failed => (IconName::XCircle, Color::Error),
        }
    }
}

#[derive(Default)]
pub(crate) struct AgentActivity {
    /// When the agent last printed something of its own.
    output_at: Option<Instant>,
    /// When the agent last rang the bell or asked for a notification.
    attention_at: Option<Instant>,
    /// When the agent seen running started; one that finished before Solo
    /// saw it run (say, restored from the last session) isn't shown as done.
    started_at: Option<Instant>,
    working_since: Option<Instant>,
    status: Option<(String, AgentStatus)>,
    /// When the pending recheck is due, and its timer.
    recheck: Option<(Instant, Task<()>)>,
    /// Turns the working icon while the agent works.
    spinner: Option<Task<()>>,
    /// What the agent last reported through a hook.
    hook: Option<HookReport>,
    /// Whether a turn reported by the agent's hooks is under way.
    in_turn: bool,
    /// The name the agent gave in its reports.
    reported_agent: Option<String>,
    /// Whether the agent finished a turn you haven't looked at since.
    unseen_finish: bool,
    /// What the agent's screen last said, for agents with screen rules.
    screen: ScreenReading,
    /// Whether the screen was read for this run (the agent has rules).
    screen_read: bool,
    screen_read_at: Option<Instant>,
}

#[derive(Clone, Copy, Debug)]
struct HookReport {
    kind: AgentHookKind,
    received_at: Instant,
    sent_at_ms: Option<u64>,
}

/// Whether a hook sent at `sent_at_ms` is newer than the last one. Hooks run
/// in the background, so one sent later can arrive first.
fn supersedes(last_sent_at_ms: Option<u64>, sent_at_ms: Option<u64>) -> bool {
    match (last_sent_at_ms, sent_at_ms) {
        (Some(last), Some(sent)) => sent >= last,
        _ => true,
    }
}

/// What an agent that reports through hooks is doing, from its last report.
///
/// Input after the report is the user answering a question (moving through
/// its choices redraws it, which counts as echo), or pressing Esc, which ends
/// a turn without a report. After that the agent works on only while it keeps
/// drawing its spinner.
fn status_from_hook(
    kind: AgentHookKind,
    reported_at: Instant,
    output_at: Option<Instant>,
    last_input_at: Option<Instant>,
    now: Instant,
) -> AgentStatus {
    let Some(input_at) = last_input_at.filter(|input_at| *input_at > reported_at) else {
        return match kind {
            AgentHookKind::PromptSubmitted => AgentStatus::Working,
            AgentHookKind::NeedsInput => AgentStatus::NeedsInput,
            AgentHookKind::SessionStarted | AgentHookKind::Stopped | AgentHookKind::Idle => {
                AgentStatus::Idle
            }
        };
    };
    let output_since_input = output_at.filter(|output_at| *output_at > input_at);
    let quiet = |since: Instant| now.duration_since(since) >= QUIET_AFTER;
    match kind {
        AgentHookKind::SessionStarted | AgentHookKind::Stopped | AgentHookKind::Idle => {
            AgentStatus::Idle
        }
        // Still choosing, or turned it down and asked what to do instead.
        AgentHookKind::NeedsInput if output_since_input.is_none() => AgentStatus::NeedsInput,
        AgentHookKind::PromptSubmitted | AgentHookKind::NeedsInput => {
            if quiet(output_since_input.unwrap_or(input_at)) {
                AgentStatus::Idle
            } else {
                AgentStatus::Working
            }
        }
    }
}

/// When a working agent's status should be looked at again if nothing else
/// happens: once it's been quiet long enough to count as stopped.
fn quiet_deadline(
    hook_received_at: Option<Instant>,
    output_at: Option<Instant>,
    last_input_at: Option<Instant>,
) -> Option<Instant> {
    let last_sign = match hook_received_at {
        None => output_at?,
        // Only input after the report makes going quiet matter.
        Some(reported_at) => {
            let input_at = last_input_at.filter(|input_at| *input_at > reported_at)?;
            output_at.map_or(input_at, |output_at| output_at.max(input_at))
        }
    };
    Some(last_sign + QUIET_AFTER)
}

/// What to show for an agent that is `status` now and was shown as
/// `previous`, and whether it has finished a turn you haven't seen: a turn
/// that ends while you aren't looking stays done until you look.
fn shown_status(
    status: AgentStatus,
    previous: Option<AgentStatus>,
    watched: bool,
    unseen_finish: bool,
) -> (AgentStatus, bool) {
    if status != AgentStatus::Idle {
        return (status, false);
    }
    let unseen_finish = unseen_finish || (previous == Some(AgentStatus::Working) && !watched);
    if unseen_finish {
        (AgentStatus::Finished, true)
    } else {
        (AgentStatus::Idle, false)
    }
}

/// What an agent is doing, from what was seen of it.
fn status_from(
    running: bool,
    exit_code: Option<i32>,
    output_at: Option<Instant>,
    attention_at: Option<Instant>,
    last_input_at: Option<Instant>,
    now: Instant,
) -> AgentStatus {
    if !running {
        return match exit_code {
            Some(code) if code != 0 => AgentStatus::Failed,
            _ => AgentStatus::Done,
        };
    }
    if let Some(attention_at) = attention_at
        && last_input_at.is_none_or(|input_at| input_at < attention_at)
        && output_at.is_none_or(|output_at| output_at <= attention_at + Duration::from_secs(1))
    {
        return AgentStatus::NeedsInput;
    }
    match output_at {
        Some(output_at) if now.duration_since(output_at) < QUIET_AFTER => AgentStatus::Working,
        _ => AgentStatus::Idle,
    }
}

impl TerminalView {
    /// The agent this terminal runs or last ran, with what it's doing.
    pub(crate) fn agent_status(&self) -> Option<(&str, AgentStatus)> {
        self.agent_activity
            .status
            .as_ref()
            .map(|(name, status)| (name.as_str(), *status))
    }

    /// The terminal printed something: if the agent did on its own, it's working.
    pub(crate) fn agent_output(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let now = Instant::now();
        let answering_input = self
            .terminal
            .read(cx)
            .last_sent_to_pty()
            .is_some_and(|input_at| now.duration_since(input_at) < ECHO_WINDOW);
        if !answering_input {
            self.agent_activity.output_at = Some(now);
        }
        self.update_agent_status(window, cx);
    }

    /// The terminal rang the bell or asked for a notification.
    pub(crate) fn agent_wants_attention(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.agent_activity.attention_at = Some(Instant::now());
        self.update_agent_status(window, cx);
    }

    /// How far the working icon has turned, from 0 to 1.
    pub(crate) fn agent_spinner_turn(&self) -> f32 {
        let Some(since) = self.agent_activity.working_since else {
            return 0.;
        };
        let step = since.elapsed().as_millis() / SPINNER_STEP.as_millis();
        (step % SPINNER_STEPS_PER_TURN) as f32 / SPINNER_STEPS_PER_TURN as f32
    }

    /// The agent's hook reported what it's doing.
    pub(crate) fn agent_hook(
        &mut self,
        hook: AgentHook,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(session_id) = hook.session_id {
            self.agent_session_reported(session_id, cx);
        }
        let activity = &mut self.agent_activity;
        let last_sent_at_ms = activity.hook.and_then(|last| last.sent_at_ms);
        if !supersedes(last_sent_at_ms, hook.sent_at_ms) {
            log::debug!("agent hook {hook:?} arrived after a later one");
            return;
        }
        if hook.agent.is_some() {
            activity.reported_agent = hook.agent.clone();
        }
        let needs_input = hook.kind == AgentHookKind::NeedsInput;
        activity.hook = Some(HookReport {
            kind: hook.kind,
            received_at: Instant::now(),
            sent_at_ms: hook.sent_at_ms,
        });
        if hook.kind == AgentHookKind::PromptSubmitted && !activity.in_turn {
            activity.in_turn = true;
            cx.emit(AgentTurnEvent::Started);
        }
        self.update_agent_status(window, cx);
        if needs_input
            && self.agent_status().map(|(_, status)| status) == Some(AgentStatus::NeedsInput)
        {
            self.notify_agent_needs_input(window, cx);
        }
    }

    /// The terminal's title changed, which agents use to say what they're
    /// doing: read the screen again now.
    pub(crate) fn agent_title_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.agent_activity.screen_read_at = None;
        self.update_agent_status(window, cx);
    }

    /// You're looking at the terminal: a turn it finished is seen.
    pub(crate) fn agent_seen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.agent_activity.unseen_finish {
            self.agent_activity.unseen_finish = false;
            self.update_agent_status(window, cx);
        }
    }

    fn is_watched(&self, window: &Window, cx: &Context<Self>) -> bool {
        window.is_window_active() && self.focus_handle.contains_focused(window, cx)
    }

    pub(crate) fn update_agent_status(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let now = Instant::now();
        let mut status = self.compute_agent_status(now, cx);
        if status.is_none() {
            // Reports are about the agent that sent them; the next one starts afresh.
            self.agent_activity.hook = None;
            self.agent_activity.reported_agent = None;
        }
        let previous = self
            .agent_activity
            .status
            .as_ref()
            .map(|(_, status)| *status);
        if let Some((_, current)) = status.as_mut() {
            let watched = self.is_watched(window, cx);
            (*current, self.agent_activity.unseen_finish) = shown_status(
                *current,
                previous,
                watched,
                self.agent_activity.unseen_finish,
            );
        } else {
            self.agent_activity.unseen_finish = false;
        }
        let new = status.as_ref().map(|(_, status)| *status);
        // Agents without hooks start a turn when their screen shows them at work.
        if new == Some(AgentStatus::Working)
            && !self.agent_activity.in_turn
            && self.agent_activity.hook.is_none()
            && self.agent_activity.screen_read
        {
            self.agent_activity.in_turn = true;
            cx.emit(AgentTurnEvent::Started);
        }
        if self.agent_activity.in_turn
            && !matches!(new, Some(AgentStatus::Working | AgentStatus::NeedsInput))
        {
            self.agent_activity.in_turn = false;
            cx.emit(AgentTurnEvent::Ended);
        }
        if new == Some(AgentStatus::Working) {
            self.agent_activity.working_since.get_or_insert(now);
        }
        if new.is_some_and(AgentStatus::is_running) {
            self.keep_agent_session(cx);
        }
        if previous != new {
            log::debug!("agent status {previous:?} -> {new:?}");
            let worked_for = match (previous, new) {
                (Some(AgentStatus::Working), Some(AgentStatus::Finished)) => self
                    .agent_activity
                    .working_since
                    .map(|since| now.duration_since(since)),
                _ => None,
            };
            if new != Some(AgentStatus::Working) {
                self.agent_activity.working_since = None;
            }
            self.agent_activity.status = status;
            if let Some(worked_for) = worked_for {
                self.notify_agent_stopped(worked_for, window, cx);
            }
            cx.emit(ItemEvent::UpdateTab);
            cx.notify();
            workspace::agent_activity::notify(cx);
        } else {
            self.agent_activity.status = status;
        }

        if new == Some(AgentStatus::Working) {
            if self.agent_activity.spinner.is_none() {
                self.agent_activity.spinner = Some(cx.spawn(async move |this, cx| {
                    loop {
                        cx.background_executor().timer(SPINNER_STEP).await;
                        if this
                            .update(cx, |_, cx| cx.emit(ItemEvent::UpdateTab))
                            .is_err()
                        {
                            break;
                        }
                    }
                }));
            }
        } else {
            self.agent_activity.spinner = None;
        }

        // Look again once it has been quiet for long enough.
        let deadline = match new {
            Some(AgentStatus::Working) => quiet_deadline(
                self.agent_activity.hook.map(|hook| hook.received_at),
                self.agent_activity.output_at,
                self.terminal.read(cx).last_sent_to_pty(),
            ),
            _ => None,
        };
        // Read the screen again once it settles after output, or once a
        // working agent's idle screen has lasted.
        let screen_deadline = if self.agent_activity.screen_read && new.is_some() {
            let after_output = self
                .agent_activity
                .output_at
                .zip(self.agent_activity.screen_read_at)
                .filter(|(output_at, read_at)| output_at >= read_at)
                .map(|(output_at, _)| output_at + SCREEN_SETTLE);
            [
                after_output,
                agent_detection::recheck_at(&self.agent_activity.screen),
            ]
            .into_iter()
            .flatten()
            .min()
        } else {
            None
        };
        let deadline = match (deadline, screen_deadline) {
            (Some(deadline), Some(screen)) => Some(deadline.min(screen)),
            (deadline, screen) => deadline.or(screen),
        };
        match deadline {
            // A recheck that comes before the deadline schedules the next one,
            // so output, which moves the deadline on, doesn't start a timer
            // each time.
            Some(deadline)
                if self
                    .agent_activity
                    .recheck
                    .as_ref()
                    .is_some_and(|(at, _)| *at <= deadline) => {}
            Some(deadline) => {
                let delay = deadline.saturating_duration_since(now) + Duration::from_millis(50);
                let task = cx.spawn_in(window, async move |this, cx| {
                    cx.background_executor().timer(delay).await;
                    this.update_in(cx, |this, window, cx| {
                        this.agent_activity.recheck = None;
                        this.update_agent_status(window, cx)
                    })
                    .ok();
                });
                self.agent_activity.recheck = Some((deadline, task));
            }
            None => self.agent_activity.recheck = None,
        }
    }

    fn compute_agent_status(
        &mut self,
        now: Instant,
        cx: &mut Context<Self>,
    ) -> Option<(String, AgentStatus)> {
        let extra = &TerminalSettings::get_global(cx).agent_commands;
        let terminal = self.terminal.read(cx);
        let last_input_at = terminal.last_sent_to_pty();
        let activity = &mut self.agent_activity;
        let block = terminal
            .shell_integration()
            .blocks()
            .iter()
            .rev()
            .find(|block| block.started_at.is_some());
        let (name, running, exit_code) = match block {
            Some(block) => {
                let command = block.command.as_deref()?;
                if block.is_running() {
                    if activity.started_at != block.started_at {
                        // Reports from a previous run don't apply to this one.
                        let reported_before = |received_at: Instant| {
                            block
                                .started_at
                                .is_some_and(|started_at| received_at < started_at)
                        };
                        if activity
                            .hook
                            .is_some_and(|hook| reported_before(hook.received_at))
                        {
                            activity.hook = None;
                            activity.reported_agent = None;
                        }
                        activity.screen = ScreenReading::default();
                        activity.screen_read = false;
                        activity.screen_read_at = None;
                    }
                    activity.started_at = block.started_at;
                } else if activity.started_at != block.started_at {
                    return None;
                }
                let name = if is_agent_command(command, extra) {
                    agent_name(command)
                } else {
                    // A program that reports its status is an agent.
                    activity.reported_agent.clone()?
                };
                (name, block.is_running(), block.exit_code)
            }
            // Without shell integration, go by the foreground process.
            None => {
                let command = terminal.foreground_process_command_name()?;
                if !is_agent_command(&command, extra) {
                    return None;
                }
                (agent_name(&command), true, None)
            }
        };
        if running {
            let due = activity
                .screen_read_at
                .is_none_or(|read_at| now.duration_since(read_at) >= SCREEN_READ_INTERVAL);
            if due {
                let program = command_program(block.and_then(|block| block.command.as_deref()))
                    .unwrap_or_else(|| name.to_lowercase());
                activity.screen_read =
                    agent_detection::read_screen(&program, terminal, &mut activity.screen, now)
                        .is_some();
                activity.screen_read_at = Some(now);
            }
        }
        let screen_status = activity
            .screen_read
            .then_some(activity.screen.status)
            .flatten();
        let status = match activity.hook {
            Some(hook) if running => {
                let status = status_from_hook(
                    hook.kind,
                    hook.received_at,
                    activity.output_at,
                    last_input_at,
                    now,
                );
                // A question plainly on screen wins over what the hook said.
                if activity.screen_read
                    && activity.screen.visible_blocker
                    && screen_status == Some(AgentStatus::NeedsInput)
                {
                    AgentStatus::NeedsInput
                } else {
                    status
                }
            }
            _ if running && screen_status.is_some() => screen_status.unwrap_or(AgentStatus::Idle),
            _ => status_from(
                running,
                exit_code,
                activity.output_at,
                activity.attention_at,
                last_input_at,
                now,
            ),
        };
        Some((name, status))
    }
}

/// The program a command runs, lowercased, to find its screen rules.
fn command_program(command: Option<&str>) -> Option<String> {
    let program = agent_name(command?);
    (!program.is_empty()).then(|| program.to_lowercase())
}

/// The agent's program name, for display.
fn agent_name(command: &str) -> String {
    let program = command
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .trim_matches(['"', '\'']);
    std::path::Path::new(program)
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| program.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turns_finished_unseen_stay_done_until_seen() {
        use AgentStatus::*;
        // Stopping while you look: idle.
        assert_eq!(
            shown_status(Idle, Some(Working), true, false),
            (Idle, false)
        );
        // Stopping in the background: done, and it stays done.
        assert_eq!(
            shown_status(Idle, Some(Working), false, false),
            (Finished, true)
        );
        assert_eq!(
            shown_status(Idle, Some(Finished), false, true),
            (Finished, true)
        );
        assert_eq!(
            shown_status(Idle, Some(Finished), true, true),
            (Finished, true)
        );
        // Once seen (`agent_seen` clears it): idle.
        assert_eq!(
            shown_status(Idle, Some(Finished), true, false),
            (Idle, false)
        );
        // A new turn, or a question, starts afresh.
        assert_eq!(
            shown_status(Working, Some(Finished), false, true),
            (Working, false)
        );
        assert_eq!(
            shown_status(NeedsInput, Some(Finished), false, true),
            (NeedsInput, false)
        );
        // Idle without a turn ending isn't done.
        assert_eq!(shown_status(Idle, None, false, false), (Idle, false));
    }

    #[test]
    fn test_status_from() {
        let start = Instant::now();
        let at = |secs: u64| start + Duration::from_secs(secs);
        // Finished.
        assert_eq!(
            status_from(false, Some(0), None, None, None, at(0)),
            AgentStatus::Done
        );
        assert_eq!(
            status_from(false, Some(2), None, None, None, at(0)),
            AgentStatus::Failed
        );
        // Printing on its own, then quiet.
        assert_eq!(
            status_from(true, None, Some(at(10)), None, Some(at(1)), at(11)),
            AgentStatus::Working
        );
        assert_eq!(
            status_from(true, None, Some(at(10)), None, Some(at(1)), at(14)),
            AgentStatus::Idle
        );
        assert_eq!(
            status_from(true, None, None, None, None, at(0)),
            AgentStatus::Idle
        );
        // Rang the bell: waiting until the user answers or it carries on.
        assert_eq!(
            status_from(true, None, Some(at(10)), Some(at(10)), Some(at(1)), at(30)),
            AgentStatus::NeedsInput
        );
        assert_eq!(
            status_from(true, None, Some(at(10)), Some(at(10)), Some(at(31)), at(32)),
            AgentStatus::Idle
        );
        assert_eq!(
            status_from(true, None, Some(at(20)), Some(at(10)), Some(at(1)), at(21)),
            AgentStatus::Working
        );
    }

    #[test]
    fn test_status_from_hook() {
        use AgentHookKind::*;
        let start = Instant::now();
        let at = |secs: u64| start + Duration::from_secs(secs);
        // Each report decides, however long the agent has been quiet.
        assert_eq!(
            status_from_hook(PromptSubmitted, at(10), None, Some(at(9)), at(100)),
            AgentStatus::Working
        );
        assert_eq!(
            status_from_hook(NeedsInput, at(10), Some(at(50)), Some(at(9)), at(100)),
            AgentStatus::NeedsInput
        );
        for kind in [SessionStarted, Stopped, Idle] {
            // Output after the report (redraws, the user typing) changes nothing.
            assert_eq!(
                status_from_hook(kind, at(10), Some(at(99)), Some(at(99)), at(100)),
                AgentStatus::Idle
            );
        }
        // Choosing, or turned down: nothing drawn since but echo.
        assert_eq!(
            status_from_hook(NeedsInput, at(10), Some(at(15)), Some(at(20)), at(40)),
            AgentStatus::NeedsInput
        );
        assert_eq!(
            status_from_hook(NeedsInput, at(10), None, Some(at(20)), at(40)),
            AgentStatus::NeedsInput
        );
        // Answered: working while it keeps drawing, then interrupted.
        assert_eq!(
            status_from_hook(NeedsInput, at(10), Some(at(21)), Some(at(20)), at(22)),
            AgentStatus::Working
        );
        assert_eq!(
            status_from_hook(NeedsInput, at(10), Some(at(30)), Some(at(20)), at(32)),
            AgentStatus::Working
        );
        assert_eq!(
            status_from_hook(NeedsInput, at(10), Some(at(30)), Some(at(20)), at(34)),
            AgentStatus::Idle
        );
        // Esc during a turn: no Stop report, but the spinner stops.
        assert_eq!(
            status_from_hook(PromptSubmitted, at(10), Some(at(19)), Some(at(20)), at(22)),
            AgentStatus::Working
        );
        assert_eq!(
            status_from_hook(PromptSubmitted, at(10), Some(at(19)), Some(at(20)), at(24)),
            AgentStatus::Idle
        );
    }

    #[test]
    fn test_quiet_deadline() {
        let start = Instant::now();
        let at = |secs: u64| start + Duration::from_secs(secs);
        // Without reports, by output alone.
        assert_eq!(
            quiet_deadline(None, Some(at(10)), Some(at(20))),
            Some(at(13))
        );
        assert_eq!(quiet_deadline(None, None, Some(at(20))), None);
        // With reports, only after input since the last one.
        assert_eq!(
            quiet_deadline(Some(at(10)), Some(at(30)), Some(at(5))),
            None
        );
        assert_eq!(
            quiet_deadline(Some(at(10)), Some(at(30)), Some(at(20))),
            Some(at(33))
        );
        assert_eq!(
            quiet_deadline(Some(at(10)), None, Some(at(20))),
            Some(at(23))
        );
    }

    #[test]
    fn test_hooks_arriving_out_of_order() {
        assert!(supersedes(None, Some(5)));
        assert!(supersedes(Some(5), None));
        assert!(supersedes(Some(5), Some(5)));
        assert!(supersedes(Some(5), Some(6)));
        assert!(!supersedes(Some(6), Some(5)));
    }

    #[test]
    fn test_agent_name() {
        assert_eq!(agent_name("claude --resume"), "claude");
        assert_eq!(agent_name(r#""C:\tools\codex.exe" exec"#), "codex");
    }
}
