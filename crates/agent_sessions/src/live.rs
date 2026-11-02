use std::time::Duration;

use gpui::{AppContext as _, Context, EventEmitter, Task};

use crate::{NewLines, SessionLocation, SessionState, TranscriptEntry, TranscriptReader};

/// How often the transcript is checked for more.
const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// SOLO: A session kept up to date as the agent appends to its transcript.
/// Reading and parsing happen in the background, and only what was added since
/// the last read.
pub struct AgentSession {
    location: SessionLocation,
    state: SessionState,
    loaded: bool,
    _poll: Task<()>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentSessionEvent {
    /// More was recorded: messages from `messages_from` on and tool calls from
    /// `tool_calls_from` on are new (or, for the last message, longer).
    Updated {
        messages_from: usize,
        tool_calls_from: usize,
    },
    /// The transcript was rewritten; everything was read again.
    Reloaded,
}

impl EventEmitter<AgentSessionEvent> for AgentSession {}

impl AgentSession {
    pub fn new(location: SessionLocation, cx: &mut Context<Self>) -> Self {
        let poll = cx.spawn({
            let path = location.path.clone();
            let adapter = location.agent.adapter();
            async move |this, cx| {
                let mut reader = TranscriptReader::new(path);
                loop {
                    let (returned_reader, read) = cx
                        .background_spawn(async move {
                            let read = reader.read_new_lines().map(|new_lines| {
                                let NewLines { restarted, lines } = new_lines;
                                let entries: Vec<TranscriptEntry> = lines
                                    .iter()
                                    .flat_map(|line| adapter.parse_line(line))
                                    .collect();
                                (restarted, entries)
                            });
                            (reader, read)
                        })
                        .await;
                    reader = returned_reader;
                    let applied = this.update(cx, |this, cx| match read {
                        Ok((restarted, entries)) => this.apply(restarted, entries, cx),
                        Err(error) => {
                            log::debug!(
                                "failed to read agent transcript {:?}: {error}",
                                reader.path()
                            );
                        }
                    });
                    if applied.is_err() {
                        break;
                    }
                    cx.background_executor().timer(POLL_INTERVAL).await;
                }
            }
        });
        Self {
            state: SessionState::new(location.agent),
            location,
            loaded: false,
            _poll: poll,
        }
    }

    fn apply(&mut self, restarted: bool, entries: Vec<TranscriptEntry>, cx: &mut Context<Self>) {
        let first_read = !self.loaded;
        self.loaded = true;
        if restarted {
            self.state = SessionState::new(self.location.agent);
        }
        if entries.is_empty() && !restarted && !first_read {
            return;
        }
        // The last message may grow with more of the same response.
        let messages_from = self.state.messages.len().saturating_sub(1);
        let tool_calls_from = self.state.tool_calls.len();
        self.state.apply_all(entries);
        cx.emit(if restarted {
            AgentSessionEvent::Reloaded
        } else {
            AgentSessionEvent::Updated {
                messages_from,
                tool_calls_from,
            }
        });
        cx.notify();
    }

    pub fn location(&self) -> &SessionLocation {
        &self.location
    }

    pub fn state(&self) -> &SessionState {
        &self.state
    }

    /// Whether the transcript has been read at least once.
    pub fn is_loaded(&self) -> bool {
        self.loaded
    }

    /// The text of the main conversation's last assistant message.
    pub fn latest_assistant_text(&self) -> Option<&str> {
        self.state
            .last_assistant_message()
            .map(|message| message.text.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AgentKind, MatchedBy};
    use gpui::TestAppContext;
    use std::io::Write as _;

    #[gpui::test]
    async fn test_follows_the_transcript(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(
            br#"{"type":"assistant","message":{"model":"claude-opus-5-5","id":"m1","content":[{"type":"text","text":"one"}]}}
"#,
        )
        .unwrap();
        file.flush().unwrap();
        let location = SessionLocation {
            agent: AgentKind::ClaudeCode,
            session_id: "s".into(),
            path,
            modified: None,
            matched_by: MatchedBy::Chosen,
        };
        let session = cx.new(|cx| AgentSession::new(location, cx));
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        cx.update(|cx| {
            let events = events.clone();
            cx.subscribe(&session, move |_, event: &AgentSessionEvent, _| {
                events.borrow_mut().push(event.clone())
            })
            .detach();
        });
        cx.run_until_parked();
        session.read_with(cx, |session, _| {
            assert!(session.is_loaded());
            assert_eq!(session.latest_assistant_text(), Some("one"));
        });

        // Half a line, then the rest.
        file.write_all(br#"{"type":"assistant","message":{"model":"claude-opus-5-5","id":"m2","#)
            .unwrap();
        file.flush().unwrap();
        cx.executor().advance_clock(POLL_INTERVAL);
        cx.run_until_parked();
        session.read_with(cx, |session, _| {
            assert_eq!(session.latest_assistant_text(), Some("one"))
        });
        file.write_all(
            br#""content":[{"type":"text","text":"two"}]}}
"#,
        )
        .unwrap();
        file.flush().unwrap();
        cx.executor().advance_clock(POLL_INTERVAL);
        cx.run_until_parked();
        session.read_with(cx, |session, _| {
            assert_eq!(session.latest_assistant_text(), Some("two"));
            assert_eq!(session.state().messages.len(), 2);
        });
        assert_eq!(
            events.borrow().last(),
            Some(&AgentSessionEvent::Updated {
                messages_from: 0,
                tool_calls_from: 0
            })
        );
    }
}
