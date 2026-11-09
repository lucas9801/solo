//! SOLO: reads what an agent is doing from its terminal's screen, with the
//! rules of the `agent_detection` crate: the bundled ones, or a user's own in
//! `<config dir>/agent-detection/<agent>.toml`, which replace them.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use agent_detection::{DetectedState, DetectionInput, Manifest};
use terminal::Terminal;

use crate::agent_status::AgentStatus;

/// A screen that read as idle right after reading as working has to read so
/// for this long, unless it plainly shows an idle prompt: agents redraw
/// between their spinner's frames.
const HOLD_WORKING: Duration = Duration::from_millis(700);

/// Where users put their own rules.
pub(crate) fn rules_directory() -> PathBuf {
    paths::config_dir().join("agent-detection")
}

fn manifests() -> &'static HashMap<String, Manifest> {
    static MANIFESTS: OnceLock<HashMap<String, Manifest>> = OnceLock::new();
    MANIFESTS.get_or_init(|| {
        let (manifests, errors) = agent_detection::load_with_overrides(&rules_directory());
        for error in errors {
            log::warn!("agent detection rules: {error}");
        }
        manifests
    })
}

/// The rules for the agent run as `program` (its program name).
fn manifest_for(program: &str) -> Option<&'static Manifest> {
    manifests()
        .values()
        .find(|manifest| manifest.is_for(program))
}

/// What an agent's screen last said, kept between readings.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ScreenReading {
    pub status: Option<AgentStatus>,
    /// The screen plainly shows a question or a permission prompt.
    pub visible_blocker: bool,
    /// The screen plainly shows the agent waiting for a prompt.
    pub visible_idle: bool,
    /// The screen plainly shows the agent at work.
    pub visible_working: bool,
    /// When the screen first read as idle while the status was working.
    idle_since: Option<Instant>,
}

/// Reads the screen of the agent run as `program`, if there are rules for
/// it, updating `reading`. `None` without rules.
pub(crate) fn read_screen(
    program: &str,
    terminal: &Terminal,
    reading: &mut ScreenReading,
    now: Instant,
) -> Option<()> {
    let manifest = manifest_for(program)?;
    let screen = terminal.agent_detection_text();
    let detection = manifest.detect(&DetectionInput {
        screen: &screen,
        osc_title: &terminal.breadcrumb_text,
        // Solo's terminal doesn't pass OSC 9 progress reports on yet.
        osc_progress: "",
    });
    *reading = next_reading(*reading, &detection, now);
    Some(())
}

fn next_reading(
    reading: ScreenReading,
    detection: &agent_detection::Detection,
    now: Instant,
) -> ScreenReading {
    if detection.skip_state_update || detection.state == DetectedState::Unknown {
        // A menu or a transcript viewer: it says nothing new.
        return reading;
    }
    let status = match detection.state {
        DetectedState::Working => AgentStatus::Working,
        DetectedState::Blocked => AgentStatus::NeedsInput,
        DetectedState::Idle | DetectedState::Unknown => AgentStatus::Idle,
    };
    let held = status == AgentStatus::Idle
        && reading.status == Some(AgentStatus::Working)
        && !detection.visible_idle
        && !detection.visible_blocker;
    if held {
        let idle_since = reading.idle_since.unwrap_or(now);
        if now.duration_since(idle_since) < HOLD_WORKING {
            return ScreenReading {
                idle_since: Some(idle_since),
                ..reading
            };
        }
    }
    ScreenReading {
        status: Some(status),
        visible_blocker: detection.visible_blocker,
        visible_idle: detection.visible_idle,
        visible_working: detection.visible_working,
        idle_since: None,
    }
}

/// When a held reading should be looked at again.
pub(crate) fn recheck_at(reading: &ScreenReading) -> Option<Instant> {
    reading.idle_since.map(|since| since + HOLD_WORKING)
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_detection::Detection;

    fn detection(state: DetectedState, visible: bool) -> Detection {
        Detection {
            state,
            skip_state_update: false,
            visible_idle: visible && state == DetectedState::Idle,
            visible_blocker: visible && state == DetectedState::Blocked,
            visible_working: visible && state == DetectedState::Working,
            rule: None,
        }
    }

    #[test]
    fn readings_hold_working_briefly() {
        let start = Instant::now();
        let working = next_reading(
            ScreenReading::default(),
            &detection(DetectedState::Working, false),
            start,
        );
        assert_eq!(working.status, Some(AgentStatus::Working));

        // A frame without the spinner doesn't end the turn...
        let held = next_reading(working, &detection(DetectedState::Idle, false), start);
        assert_eq!(held.status, Some(AgentStatus::Working));
        assert_eq!(recheck_at(&held), Some(start + HOLD_WORKING));
        // ...unless it lasts,
        let idle = next_reading(
            held,
            &detection(DetectedState::Idle, false),
            start + HOLD_WORKING,
        );
        assert_eq!(idle.status, Some(AgentStatus::Idle));
        assert_eq!(recheck_at(&idle), None);
        // or the idle prompt is plain to see.
        let shown = next_reading(working, &detection(DetectedState::Idle, true), start);
        assert_eq!(shown.status, Some(AgentStatus::Idle));

        let blocked = next_reading(working, &detection(DetectedState::Blocked, true), start);
        assert_eq!(blocked.status, Some(AgentStatus::NeedsInput));
        assert!(blocked.visible_blocker);

        // Menus and viewers say nothing.
        let mut menu = detection(DetectedState::Unknown, false);
        menu.skip_state_update = true;
        assert_eq!(
            next_reading(blocked, &menu, start).status,
            Some(AgentStatus::NeedsInput)
        );
    }

    #[test]
    fn agents_have_rules() {
        for program in [
            "claude",
            "codex",
            "opencode",
            "copilot",
            "gemini",
            "cursor-agent",
        ] {
            assert!(manifest_for(program).is_some(), "{program}");
        }
        assert!(manifest_for("bash").is_none());
    }
}
