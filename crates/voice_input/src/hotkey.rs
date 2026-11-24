//! The shortcut's states: a press that is let go within half a second starts
//! listening until the next press (toggle); a press held longer listens until
//! it is let go (push to talk). Escape cancels either.

use std::time::{Duration, Instant};

pub const HOLD_THRESHOLD: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// The shortcut went down (key repeats are left out by the caller).
    Press,
    /// The shortcut's key came up.
    Release,
    Escape,
}

/// What the event asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    None,
    Start,
    /// Stop listening and recognize.
    Stop,
    /// Stop listening and throw the sound away.
    Cancel,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum State {
    #[default]
    Idle,
    /// The shortcut is down since this moment.
    Held(Instant),
    /// Short press done; listening until the next press.
    Toggled,
}

#[derive(Debug, Default)]
pub struct Hotkey {
    state: State,
}

impl Hotkey {
    pub fn is_idle(&self) -> bool {
        self.state == State::Idle
    }

    /// Whether the shortcut is physically down, so its release matters.
    pub fn is_held(&self) -> bool {
        matches!(self.state, State::Held(_))
    }

    pub fn reset(&mut self) {
        self.state = State::Idle;
    }

    pub fn handle(&mut self, event: Event, now: Instant) -> Effect {
        match (self.state, event) {
            (State::Idle, Event::Press) => {
                self.state = State::Held(now);
                Effect::Start
            }
            (State::Toggled, Event::Press) => {
                self.state = State::Idle;
                Effect::Stop
            }
            (State::Held(since), Event::Release) => {
                if now.saturating_duration_since(since) >= HOLD_THRESHOLD {
                    self.state = State::Idle;
                    Effect::Stop
                } else {
                    self.state = State::Toggled;
                    Effect::None
                }
            }
            (State::Held(_) | State::Toggled, Event::Escape) => {
                self.state = State::Idle;
                Effect::Cancel
            }
            _ => Effect::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tap_starts_and_the_second_tap_stops() {
        let start = Instant::now();
        let mut hotkey = Hotkey::default();
        assert_eq!(hotkey.handle(Event::Press, start), Effect::Start);
        assert_eq!(
            hotkey.handle(Event::Release, start + Duration::from_millis(120)),
            Effect::None
        );
        assert!(!hotkey.is_idle());
        assert_eq!(
            hotkey.handle(Event::Press, start + Duration::from_secs(3)),
            Effect::Stop
        );
        assert_eq!(
            hotkey.handle(Event::Release, start + Duration::from_secs(3)),
            Effect::None
        );
        assert!(hotkey.is_idle());
    }

    #[test]
    fn test_hold_listens_until_release() {
        let start = Instant::now();
        let mut hotkey = Hotkey::default();
        assert_eq!(hotkey.handle(Event::Press, start), Effect::Start);
        assert_eq!(
            hotkey.handle(Event::Release, start + Duration::from_millis(900)),
            Effect::Stop
        );
        assert!(hotkey.is_idle());
    }

    #[test]
    fn test_exactly_half_a_second_counts_as_hold() {
        let start = Instant::now();
        let mut hotkey = Hotkey::default();
        hotkey.handle(Event::Press, start);
        assert_eq!(
            hotkey.handle(Event::Release, start + HOLD_THRESHOLD),
            Effect::Stop
        );
    }

    #[test]
    fn test_escape_cancels_while_held_and_while_toggled() {
        let start = Instant::now();
        let mut hotkey = Hotkey::default();
        hotkey.handle(Event::Press, start);
        assert_eq!(hotkey.handle(Event::Escape, start), Effect::Cancel);
        assert!(hotkey.is_idle());

        hotkey.handle(Event::Press, start);
        hotkey.handle(Event::Release, start + Duration::from_millis(100));
        assert_eq!(hotkey.handle(Event::Escape, start), Effect::Cancel);
        assert!(hotkey.is_idle());
    }

    #[test]
    fn test_escape_and_release_when_idle_do_nothing() {
        let mut hotkey = Hotkey::default();
        assert_eq!(hotkey.handle(Event::Escape, Instant::now()), Effect::None);
        assert_eq!(hotkey.handle(Event::Release, Instant::now()), Effect::None);
        assert!(hotkey.is_idle());
    }

    #[test]
    fn test_a_repeat_press_while_held_does_not_restart() {
        let start = Instant::now();
        let mut hotkey = Hotkey::default();
        hotkey.handle(Event::Press, start);
        assert_eq!(
            hotkey.handle(Event::Press, start + Duration::from_millis(50)),
            Effect::None
        );
        assert!(hotkey.is_held());
    }
}
