//! SOLO: what an AI agent running in a terminal (Claude Code, Codex, ...) is
//! doing, shown on its tab: working, waiting for you, idle, done or failed.
//!
//! Agents don't report it, so it's read from the terminal: they animate a
//! spinner while they work and go quiet when they stop, and ring the bell or
//! ask for a notification when they need the user. Output right after input
//! (echoed keys, redraws after a resize or a focus change) doesn't count as
//! working.

use std::time::{Duration, Instant};

use gpui::{Context, Task, Window};
use settings::Settings;
use terminal::terminal_settings::TerminalSettings;
use ui::{Color, IconName};
use workspace::item::ItemEvent;

use crate::{TerminalView, command_blocks::is_agent_command};

/// Output this soon after input is taken as the program answering it.
const ECHO_WINDOW: Duration = Duration::from_millis(400);
/// An agent that printed nothing of its own for this long has stopped working.
const QUIET_AFTER: Duration = Duration::from_secs(3);
/// Notify when an agent stops after working at least this long in the background.
pub(crate) const NOTIFY_AFTER_WORKING: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentStatus {
    Working,
    /// The agent rang the bell or asked for a notification, and nothing has
    /// happened since.
    NeedsInput,
    /// Running but quiet, likely waiting for the next prompt.
    Idle,
    Done,
    Failed,
}

impl AgentStatus {
    pub(crate) fn label(self) -> &'static str {
        match self {
            AgentStatus::Working => "Working",
            AgentStatus::NeedsInput => "Needs your input",
            AgentStatus::Idle => "Idle",
            AgentStatus::Done => "Done",
            AgentStatus::Failed => "Failed",
        }
    }

    pub(crate) fn icon(self) -> (IconName, Color) {
        match self {
            AgentStatus::Working => (IconName::LoadCircle, Color::Accent),
            AgentStatus::NeedsInput => (IconName::Bell, Color::Warning),
            AgentStatus::Idle => (IconName::Circle, Color::Muted),
            AgentStatus::Done => (IconName::Check, Color::Success),
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
    recheck: Option<Task<()>>,
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

    pub(crate) fn update_agent_status(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let now = Instant::now();
        let status = self.compute_agent_status(now, cx);
        let previous = self.agent_activity.status.as_ref().map(|(_, status)| *status);
        let new = status.as_ref().map(|(_, status)| *status);
        if new == Some(AgentStatus::Working) {
            self.agent_activity.working_since.get_or_insert(now);
        }
        if previous != new {
            log::debug!("agent status {previous:?} -> {new:?}");
            let worked_for = match (previous, new) {
                (Some(AgentStatus::Working), Some(AgentStatus::Idle)) => self
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
        } else {
            self.agent_activity.status = status;
        }

        // Look again once it has been quiet for long enough.
        self.agent_activity.recheck = match (new, self.agent_activity.output_at) {
            (Some(AgentStatus::Working), Some(output_at)) => {
                let delay = (output_at + QUIET_AFTER).saturating_duration_since(now)
                    + Duration::from_millis(50);
                Some(cx.spawn_in(window, async move |this, cx| {
                    cx.background_executor().timer(delay).await;
                    this.update_in(cx, |this, window, cx| this.update_agent_status(window, cx))
                        .ok();
                }))
            }
            _ => None,
        };
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
                if !is_agent_command(command, extra) {
                    return None;
                }
                if block.is_running() {
                    activity.started_at = block.started_at;
                } else if activity.started_at != block.started_at {
                    return None;
                }
                (agent_name(command), block.is_running(), block.exit_code)
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
        let status = status_from(
            running,
            exit_code,
            activity.output_at,
            activity.attention_at,
            last_input_at,
            now,
        );
        Some((name, status))
    }
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
    fn test_agent_name() {
        assert_eq!(agent_name("claude --resume"), "claude");
        assert_eq!(agent_name(r#""C:\tools\codex.exe" exec"#), "codex");
    }
}
