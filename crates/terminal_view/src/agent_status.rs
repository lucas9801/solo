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

use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui::{Context, EventEmitter, Task, Window};
use settings::Settings;
use terminal::UserInput;
use terminal::shell_integration::{AgentHook, AgentHookKind};
use terminal::terminal_settings::TerminalSettings;
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
/// SOLO: the ring around an agent waiting for you breathes, in steps too, and
/// slower; after a while it stops, so an agent left waiting doesn't keep
/// redrawing.
const BREATH_STEP: Duration = Duration::from_millis(250);
const BREATH_STEPS: u128 = 8;
const BREATHE_FOR: Duration = Duration::from_secs(120);
/// Notify when an agent stops after working at least this long in the background.
pub(crate) const NOTIFY_AFTER_WORKING: Duration = Duration::from_secs(10);
/// An agent's screen isn't read for this long after you scroll it: it shows
/// its history then, not what it's doing.
const SCROLL_HOLD: Duration = Duration::from_secs(2);

/// SOLO: what the agent is doing; shown alike on its tab, its workspace's tab
/// and the agent board.
pub(crate) use workspace::agent_activity::AgentState as AgentStatus;

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
    /// When the agent started waiting for you.
    waiting_since: Option<Instant>,
    status: Option<(String, AgentStatus)>,
    /// When the pending recheck is due, and its timer.
    recheck: Option<(Instant, Task<()>)>,
    /// Animates the status icon while the agent works or waits for you, for
    /// that status.
    spinner: Option<(AgentStatus, Task<()>)>,
    /// What the agent last reported through a hook.
    hook: Option<HookReport>,
    /// Whether a turn reported by the agent's hooks is under way.
    in_turn: bool,
    /// The name the agent gave in its reports.
    reported_agent: Option<String>,
    /// Where the agent said it works, in its reports.
    reported_directory: Option<PathBuf>,
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

/// The user input that can end what an agent reported through a hook without
/// another report: answering its question (keys or a click), or interrupting
/// a turn (Esc or Ctrl+C), which agents don't report. Scrolling, switching
/// windows and resizing don't count, nor does typing the next prompt while
/// it works.
fn answer_to_hook(kind: AgentHookKind, user_input: UserInput) -> Option<Instant> {
    match kind {
        AgentHookKind::NeedsInput => user_input.typed_at,
        AgentHookKind::PromptSubmitted => user_input.interrupted_at,
        AgentHookKind::SessionStarted | AgentHookKind::Stopped | AgentHookKind::Idle => None,
    }
}

/// What an agent that reports through hooks is doing, from its last report.
///
/// An answer after the report (see [`answer_to_hook`]; moving through a
/// question's choices redraws it, which counts as echo) ends it without a
/// report. After that the agent works on only while it keeps drawing its
/// spinner.
fn status_from_hook(
    kind: AgentHookKind,
    reported_at: Instant,
    output_at: Option<Instant>,
    user_input: UserInput,
    now: Instant,
) -> AgentStatus {
    let Some(input_at) =
        answer_to_hook(kind, user_input).filter(|input_at| *input_at > reported_at)
    else {
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
    hook: Option<(AgentHookKind, Instant)>,
    output_at: Option<Instant>,
    user_input: UserInput,
) -> Option<Instant> {
    let last_sign = match hook {
        None => output_at?,
        // Only an answer after the report makes going quiet matter.
        Some((kind, reported_at)) => {
            let input_at =
                answer_to_hook(kind, user_input).filter(|input_at| *input_at > reported_at)?;
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
    typed_at: Option<Instant>,
    now: Instant,
) -> AgentStatus {
    if !running {
        return match exit_code {
            Some(code) if code != 0 => AgentStatus::Failed,
            _ => AgentStatus::Done,
        };
    }
    if let Some(attention_at) = attention_at
        && typed_at.is_none_or(|typed_at| typed_at < attention_at)
        && output_at.is_none_or(|output_at| output_at <= attention_at + Duration::from_secs(1))
    {
        return AgentStatus::NeedsInput;
    }
    match output_at {
        Some(output_at) if now.duration_since(output_at) < QUIET_AFTER => AgentStatus::Working,
        _ => AgentStatus::Idle,
    }
}

impl AgentActivity {
    /// Whether the agent's stop is one it reported through a hook, or its
    /// screen plainly shows an idle prompt.
    fn stop_confirmed(&self) -> bool {
        match self.hook {
            Some(hook) => matches!(
                hook.kind,
                AgentHookKind::Stopped | AgentHookKind::Idle | AgentHookKind::SessionStarted
            ),
            None => self.screen_read && self.screen.visible_idle,
        }
    }
}

impl TerminalView {
    /// Where the agent running in this terminal said it works, which can
    /// differ from where its shell is (a session resumed from another
    /// folder, say).
    pub(crate) fn agent_reported_directory(&self) -> Option<&PathBuf> {
        self.agent_activity.reported_directory.as_ref()
    }

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

    /// How far the working icon has turned, or the waiting ring has
    /// breathed, from 0 to 1; `None` when the status icon stands still.
    pub(crate) fn agent_status_phase(&self) -> Option<f32> {
        let (_, status) = self.agent_status()?;
        let (since, step, steps) = match status {
            AgentStatus::Working => (
                self.agent_activity.working_since?,
                SPINNER_STEP,
                SPINNER_STEPS_PER_TURN,
            ),
            AgentStatus::NeedsInput => {
                let since = self.agent_activity.waiting_since?;
                if since.elapsed() >= BREATHE_FOR {
                    return None;
                }
                (since, BREATH_STEP, BREATH_STEPS)
            }
            _ => return None,
        };
        let step = since.elapsed().as_millis() / step.as_millis();
        Some((step % steps) as f32 / steps as f32)
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
        // SOLO: kept to resume that session by its id after a restart.
        if let Some(session_id) = hook.raw_session_id.clone() {
            self.agent_resume_session_reported(session_id, cx);
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
        if hook.working_directory.is_some() {
            activity.reported_directory = hook.working_directory.clone();
        }
        let needs_input = hook.kind == AgentHookKind::NeedsInput;
        // What the screen says now, to check the report against.
        activity.screen_read_at = None;
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
            self.agent_activity.reported_directory = None;
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
        if new == Some(AgentStatus::NeedsInput) {
            self.agent_activity.waiting_since.get_or_insert(now);
        } else {
            self.agent_activity.waiting_since = None;
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
            // Only a stop the agent reported, or its screen plainly shows: one
            // that's only inferred from quiet may be wrong.
            if let Some(worked_for) = worked_for
                && self.agent_activity.stop_confirmed()
            {
                self.notify_agent_stopped(worked_for, window, cx);
            }
            cx.emit(ItemEvent::UpdateTab);
            cx.notify();
            workspace::agent_activity::notify(cx);
        } else {
            self.agent_activity.status = status;
        }

        let animation_step = match new {
            Some(AgentStatus::Working) => Some(SPINNER_STEP),
            Some(AgentStatus::NeedsInput) => Some(BREATH_STEP),
            _ => None,
        };
        match (new, animation_step) {
            (Some(new), Some(step)) => {
                if self
                    .agent_activity
                    .spinner
                    .as_ref()
                    .is_none_or(|(animated, _)| *animated != new)
                {
                    let task = cx.spawn(async move |this, cx| {
                        loop {
                            cx.background_executor().timer(step).await;
                            let animating = this.update(cx, |this, cx| {
                                cx.emit(ItemEvent::UpdateTab);
                                this.agent_status_phase().is_some()
                            });
                            if !matches!(animating, Ok(true)) {
                                break;
                            }
                        }
                    });
                    self.agent_activity.spinner = Some((new, task));
                }
            }
            _ => self.agent_activity.spinner = None,
        }

        // Look again once it has been quiet for long enough.
        let deadline = match new {
            Some(AgentStatus::Working) => quiet_deadline(
                self.agent_activity
                    .hook
                    .map(|hook| (hook.kind, hook.received_at)),
                self.agent_activity.output_at,
                self.terminal.read(cx).user_input(),
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
            let after_scrolling = self
                .terminal
                .read(cx)
                .user_input()
                .scrolled_at
                .map(|scrolled_at| scrolled_at + SCROLL_HOLD)
                .filter(|at| *at > now);
            [
                after_output,
                after_scrolling,
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
        let user_input = terminal.user_input();
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
                            activity.reported_directory = None;
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
            // A screen you just scrolled shows the agent's history: keep what
            // it said before, unless it's the first reading of this run.
            let scrolled = user_input
                .scrolled_at
                .is_some_and(|scrolled_at| now.duration_since(scrolled_at) < SCROLL_HOLD);
            if due && !(scrolled && activity.screen_read) {
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
                    user_input,
                    now,
                );
                // A question plainly on screen wins over what the hook said.
                if activity.screen_read
                    && activity.screen.visible_blocker
                    && screen_status == Some(AgentStatus::NeedsInput)
                {
                    AgentStatus::NeedsInput
                } else if status == AgentStatus::Idle
                    && activity.screen_read
                    && activity.screen.visible_working
                    && screen_status == Some(AgentStatus::Working)
                {
                    // So does work plainly on screen ("esc to interrupt"):
                    // a report or a quiet spell that said it stopped was about
                    // something else.
                    AgentStatus::Working
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
                user_input.typed_at,
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

    fn typed(at: Instant) -> UserInput {
        UserInput {
            typed_at: Some(at),
            ..UserInput::default()
        }
    }

    fn interrupted(at: Instant) -> UserInput {
        UserInput {
            typed_at: Some(at),
            interrupted_at: Some(at),
            ..UserInput::default()
        }
    }

    #[test]
    fn test_status_from_hook() {
        use AgentHookKind::*;
        let start = Instant::now();
        let at = |secs: u64| start + Duration::from_secs(secs);
        // Each report decides, however long the agent has been quiet.
        assert_eq!(
            status_from_hook(PromptSubmitted, at(10), None, typed(at(9)), at(100)),
            AgentStatus::Working
        );
        assert_eq!(
            status_from_hook(NeedsInput, at(10), Some(at(50)), typed(at(9)), at(100)),
            AgentStatus::NeedsInput
        );
        for kind in [SessionStarted, Stopped, Idle] {
            // Output after the report (redraws, the user typing) changes nothing.
            assert_eq!(
                status_from_hook(kind, at(10), Some(at(99)), interrupted(at(99)), at(100)),
                AgentStatus::Idle
            );
        }
        // Choosing, or turned down: nothing drawn since but echo.
        assert_eq!(
            status_from_hook(NeedsInput, at(10), Some(at(15)), typed(at(20)), at(40)),
            AgentStatus::NeedsInput
        );
        assert_eq!(
            status_from_hook(NeedsInput, at(10), None, typed(at(20)), at(40)),
            AgentStatus::NeedsInput
        );
        // Answered: working while it keeps drawing, then interrupted.
        assert_eq!(
            status_from_hook(NeedsInput, at(10), Some(at(21)), typed(at(20)), at(22)),
            AgentStatus::Working
        );
        assert_eq!(
            status_from_hook(NeedsInput, at(10), Some(at(30)), typed(at(20)), at(32)),
            AgentStatus::Working
        );
        assert_eq!(
            status_from_hook(NeedsInput, at(10), Some(at(30)), typed(at(20)), at(34)),
            AgentStatus::Idle
        );
        // Esc during a turn: no Stop report, but the spinner stops.
        assert_eq!(
            status_from_hook(
                PromptSubmitted,
                at(10),
                Some(at(19)),
                interrupted(at(20)),
                at(22)
            ),
            AgentStatus::Working
        );
        assert_eq!(
            status_from_hook(
                PromptSubmitted,
                at(10),
                Some(at(19)),
                interrupted(at(20)),
                at(24)
            ),
            AgentStatus::Idle
        );
    }

    #[test]
    fn test_scrolling_and_typing_dont_end_a_turn() {
        use AgentHookKind::*;
        let start = Instant::now();
        let at = |secs: u64| start + Duration::from_secs(secs);
        // Scrolling the agent's screen (which may stop it redrawing) and
        // switching windows during a turn: still working, however quiet.
        let scrolled = UserInput {
            scrolled_at: Some(at(20)),
            ..UserInput::default()
        };
        assert_eq!(
            status_from_hook(PromptSubmitted, at(10), Some(at(19)), scrolled, at(60)),
            AgentStatus::Working
        );
        assert_eq!(
            quiet_deadline(Some((PromptSubmitted, at(10))), Some(at(19)), scrolled),
            None
        );
        // Typing the next prompt while it works.
        assert_eq!(
            status_from_hook(PromptSubmitted, at(10), Some(at(19)), typed(at(20)), at(60)),
            AgentStatus::Working
        );
        // Scrolling doesn't answer a question either.
        assert_eq!(
            status_from_hook(NeedsInput, at(10), Some(at(21)), scrolled, at(60)),
            AgentStatus::NeedsInput
        );
    }

    #[test]
    fn test_quiet_deadline() {
        use AgentHookKind::*;
        let start = Instant::now();
        let at = |secs: u64| start + Duration::from_secs(secs);
        // Without reports, by output alone.
        assert_eq!(
            quiet_deadline(None, Some(at(10)), typed(at(20))),
            Some(at(13))
        );
        assert_eq!(quiet_deadline(None, None, typed(at(20))), None);
        // With reports, only after an answer since the last one.
        assert_eq!(
            quiet_deadline(Some((NeedsInput, at(10))), Some(at(30)), typed(at(5))),
            None
        );
        assert_eq!(
            quiet_deadline(Some((NeedsInput, at(10))), Some(at(30)), typed(at(20))),
            Some(at(33))
        );
        assert_eq!(
            quiet_deadline(Some((NeedsInput, at(10))), None, typed(at(20))),
            Some(at(23))
        );
        assert_eq!(
            quiet_deadline(Some((PromptSubmitted, at(10))), None, interrupted(at(20))),
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
