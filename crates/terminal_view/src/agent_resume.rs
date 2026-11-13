//! SOLO: offering, after a restart, to continue the agent session (Claude
//! Code, Codex, ...) a terminal was running when Solo quit.
//!
//! When an agent command starts, the terminal saves it with its other state;
//! when the agent exits by itself, it's cleared. One still saved when the
//! terminal is restored was running when Solo quit, and a bar above the
//! terminal offers to type the agent's resume command.

use std::{path::PathBuf, time::Instant};

use gpui::{AnyElement, Context, Focusable as _, Window};
use serde::{Deserialize, Serialize};
use settings::Settings as _;
use terminal::{Modes, terminal_settings::TerminalSettings};
use ui::{Button, ButtonStyle, prelude::*};
use util::ResultExt as _;

use crate::{TerminalView, command_blocks::is_agent_command, persistence::TerminalDb};

/// An agent command a terminal ran, as saved.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AgentSession {
    pub command: String,
    pub working_directory: Option<PathBuf>,
    /// The session id the agent's hook reported, for agents that resume a
    /// session by its id (see [`RESUME_BY_ID`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

impl AgentSession {
    pub(crate) fn to_json(&self) -> Option<String> {
        serde_json::to_string(self).log_err()
    }

    pub(crate) fn from_json(json: &str) -> Option<Self> {
        serde_json::from_str(json).log_err()
    }
}

#[derive(Default)]
pub(crate) struct AgentResumeState {
    /// When the agent command this terminal saved started.
    recorded: Option<Instant>,
    /// The agent session saved for that command.
    current: Option<AgentSession>,
    /// The session from before the restart, offered until acted on.
    offer: Option<AgentSession>,
}

/// What resumes each built-in agent's most recent session, from each one's
/// documentation. Agents not listed get no resume button.
const RESUME_ARGUMENTS: &[(&str, &str)] = &[
    // `claude --help`: "-c, --continue  Continue the most recent conversation".
    ("claude", "--continue"),
    // Codex CLI reference: `codex resume --last` resumes the latest session
    // in the current directory.
    ("codex", "resume --last"),
    // Gemini CLI session management: `--resume` loads the most recent session.
    ("gemini", "--resume"),
    // opencode CLI: `--continue` continues the last session.
    ("opencode", "--continue"),
    // GitHub Copilot CLI: `copilot --continue` resumes the most recent session.
    ("copilot", "--continue"),
    // Cursor CLI: `resume` resumes the latest conversation.
    ("cursor-agent", "resume"),
    // aider options: `--restore-chat-history` restores the previous chat.
    ("aider", "--restore-chat-history"),
];

/// What resumes a session by its id, for the agents whose hook reports a
/// session id: with several sessions on one server, OpenCode's "most recent"
/// may be another terminal's.
const RESUME_BY_ID: &[(&str, &str)] = &[
    // OpenCode CLI (https://docs.opencode.ai/docs/cli/): `--session <id>`,
    // "Session ID to continue". OpenCode 2 documents it for `opencode mini`
    // and `opencode run` (https://opencode.ai/docs/cli/commands/).
    ("opencode", "--session"),
];

/// The program of `command` as typed, and its name, lowercase.
fn program(command: &str) -> Option<(&str, String)> {
    let program = command.split_whitespace().next()?;
    let name = std::path::Path::new(program.trim_matches(['"', '\'']))
        .file_stem()?
        .to_string_lossy()
        .to_lowercase();
    Some((program, name))
}

/// The agent's name as shown, from its command.
pub(crate) fn agent_display_name(command: &str) -> String {
    program(command)
        .map(|(_, name)| name)
        .unwrap_or_else(|| command.trim().to_string())
}

/// The command that continues the session of the agent `command` ran: the
/// one with `session_id` if the agent resumes by id, else its most recent
/// one. It uses the same program (so a full path or a wrapper name still
/// works).
pub(crate) fn resume_command(command: &str, session_id: Option<&str>) -> Option<String> {
    let (program, name) = program(command)?;
    let by_id = session_id
        .filter(|id| terminal::shell_integration::is_session_id(id))
        .and_then(|id| {
            RESUME_BY_ID
                .iter()
                .find(|(agent, _)| *agent == name)
                .map(|(_, flag)| format!("{program} {flag} {id}"))
        });
    if by_id.is_some() {
        return by_id;
    }
    RESUME_ARGUMENTS
        .iter()
        .find(|(agent, _)| *agent == name)
        .map(|(_, arguments)| format!("{program} {arguments}"))
}

impl TerminalView {
    /// A command started: saves it if it's an agent, to offer continuing it
    /// should Solo quit while it runs.
    pub(crate) fn record_agent_session(&mut self, cx: &mut Context<Self>) {
        let extra = &TerminalSettings::get_global(cx).agent_commands;
        let terminal = self.terminal.read(cx);
        if terminal.task().is_some() {
            return;
        }
        let Some(block) = terminal
            .shell_integration()
            .blocks()
            .last()
            .filter(|block| block.is_running() && block.started_at.is_some())
        else {
            return;
        };
        let Some(command) = block
            .command
            .as_deref()
            .filter(|command| is_agent_command(command, extra))
        else {
            return;
        };
        if self.agent_resume.recorded == block.started_at {
            return;
        }
        let session = AgentSession {
            command: command.trim().to_string(),
            working_directory: block
                .working_directory
                .clone()
                .or_else(|| terminal.working_directory()),
            session_id: None,
        };
        self.agent_resume.recorded = block.started_at;
        self.agent_resume.current = Some(session.clone());
        // Started again by hand: nothing left to offer.
        if self.agent_resume.offer.take().is_some() {
            cx.notify();
        }
        self.save_agent_session(Some(session), cx);
    }

    /// A command finished: if it was the saved agent, it exited by itself
    /// and there's nothing to continue.
    pub(crate) fn clear_finished_agent_session(&mut self, cx: &mut Context<Self>) {
        let Some(recorded) = self.agent_resume.recorded else {
            return;
        };
        let still_running = self
            .terminal
            .read(cx)
            .shell_integration()
            .blocks()
            .iter()
            .rev()
            .find(|block| block.started_at == Some(recorded))
            .is_some_and(|block| block.is_running());
        if still_running {
            return;
        }
        self.agent_resume.recorded = None;
        self.agent_resume.current = None;
        self.save_agent_session(None, cx);
    }

    /// The running agent's hook reported its session: saved with the agent
    /// command, for agents that resume a session by its id.
    pub(crate) fn agent_resume_session_reported(
        &mut self,
        session_id: String,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.agent_resume.current.as_mut() else {
            return;
        };
        let resumes_by_id = program(&session.command)
            .is_some_and(|(_, name)| RESUME_BY_ID.iter().any(|(agent, _)| *agent == name));
        if !resumes_by_id || session.session_id.as_deref() == Some(session_id.as_str()) {
            return;
        }
        session.session_id = Some(session_id);
        let session = session.clone();
        self.save_agent_session(Some(session), cx);
    }

    /// Offers to continue `session`, saved before the restart, and keeps it
    /// saved under this terminal until it's acted on.
    pub(crate) fn offer_agent_session(&mut self, session: AgentSession, cx: &mut Context<Self>) {
        self.save_agent_session(Some(session.clone()), cx);
        self.agent_resume.offer = Some(session);
        cx.notify();
    }

    fn save_agent_session(&self, session: Option<AgentSession>, cx: &mut Context<Self>) {
        let Some(workspace_id) = self.workspace_id else {
            return;
        };
        let item_id = cx.entity_id().as_u64();
        let json = session.and_then(|session| session.to_json());
        let db = TerminalDb::global(cx);
        cx.background_spawn(async move {
            db.save_agent_session(item_id, workspace_id, json)
                .await
                .log_err();
        })
        .detach();
    }

    fn continue_agent_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(command) =
            self.agent_resume.offer.take().and_then(|session| {
                resume_command(&session.command, session.session_id.as_deref())
            })
        else {
            return;
        };
        cx.notify();
        let terminal = self.terminal.read(cx);
        let at_prompt = !terminal
            .shell_integration()
            .blocks()
            .last()
            .is_some_and(|block| block.is_running())
            && !terminal.last_content.mode.contains(Modes::ALT_SCREEN);
        // At the prompt typing goes to the input editor, when it shows.
        if let Some(input) = self
            .input
            .clone()
            .filter(|_| at_prompt && TerminalSettings::get_global(cx).input_editor)
        {
            let editor = input.read(cx).editor().clone();
            editor.update(cx, |editor, cx| {
                editor.set_text(command, window, cx);
                editor.move_to_end(&editor::actions::MoveToEnd, window, cx);
            });
            window.focus(&editor.focus_handle(cx), cx);
        } else {
            // Typed, not run: the user presses Enter.
            self.terminal
                .update(cx, |terminal, _| terminal.input(command.into_bytes()));
            window.focus(&self.focus_handle, cx);
        }
    }

    fn dismiss_agent_session(&mut self, cx: &mut Context<Self>) {
        if self.agent_resume.offer.take().is_some() {
            self.save_agent_session(None, cx);
            cx.notify();
        }
    }

    /// The bar above the terminal offering to continue the session.
    pub(crate) fn render_agent_resume_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let session = self.agent_resume.offer.as_ref()?;
        let name = agent_display_name(&session.command);
        let can_continue =
            resume_command(&session.command, session.session_id.as_deref()).is_some();
        let text = if util::i18n::is_chinese() {
            format!("上次在这里运行过 {name}")
        } else {
            format!("{name} was running here last time")
        };
        Some(
            h_flex()
                .id("agent-resume-bar")
                .w_full()
                .flex_none()
                .gap_2()
                .px_2()
                .py_1()
                .bg(cx.theme().colors().editor_subheader_background)
                .border_b_1()
                .border_color(cx.theme().colors().border_variant)
                .child(
                    Icon::new(IconName::HistoryRerun)
                        .size(IconSize::Small)
                        .color(Color::Muted),
                )
                .child(Label::new(text).size(LabelSize::Small).truncate())
                .child(div().flex_1())
                .when(can_continue, |bar| {
                    bar.child(
                        Button::new(
                            "continue-agent-session",
                            util::i18n::tr("Continue Session", "继续会话"),
                        )
                        .style(ButtonStyle::Filled)
                        .label_size(LabelSize::Small)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.continue_agent_session(window, cx)
                        })),
                    )
                })
                .child(
                    Button::new("dismiss-agent-session", util::i18n::tr("Dismiss", "忽略"))
                        .label_size(LabelSize::Small)
                        .on_click(cx.listener(|this, _, _, cx| this.dismiss_agent_session(cx))),
                )
                .into_any_element(),
        )
    }
}

/// Reads the agent session saved for a terminal, if any.
pub(crate) fn saved_agent_session(
    db: &TerminalDb,
    item_id: workspace::ItemId,
    workspace_id: workspace::WorkspaceId,
) -> Option<AgentSession> {
    db.get_agent_session(item_id, workspace_id)
        .log_err()
        .flatten()
        // Reads as empty when nothing is saved.
        .filter(|json| !json.trim().is_empty())
        .as_deref()
        .and_then(AgentSession::from_json)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resume_command() {
        assert_eq!(
            resume_command("claude", None).as_deref(),
            Some("claude --continue")
        );
        assert_eq!(
            resume_command("claude --model opus", None).as_deref(),
            Some("claude --continue")
        );
        assert_eq!(
            resume_command(r"C:\tools\Claude.exe", None).as_deref(),
            Some(r"C:\tools\Claude.exe --continue")
        );
        assert_eq!(
            resume_command("codex", None).as_deref(),
            Some("codex resume --last")
        );
        assert_eq!(
            resume_command("gemini", None).as_deref(),
            Some("gemini --resume")
        );
        assert_eq!(
            resume_command("cursor-agent", None).as_deref(),
            Some("cursor-agent resume")
        );
        // Not confirmed in their documentation.
        assert_eq!(resume_command("qwen", None), None);
        assert_eq!(resume_command("amp", None), None);
        assert_eq!(resume_command("my-agent", None), None);
        assert_eq!(resume_command("", None), None);
    }

    #[test]
    fn test_resume_command_by_session_id() {
        assert_eq!(
            resume_command("opencode", Some("ses_4f2a1cB9")).as_deref(),
            Some("opencode --session ses_4f2a1cB9")
        );
        assert_eq!(
            resume_command(r"C:\tools\opencode.cmd --model x", Some("ses_1")).as_deref(),
            Some(r"C:\tools\opencode.cmd --session ses_1")
        );
        // No id yet, or one that isn't: the most recent session.
        assert_eq!(
            resume_command("opencode", None).as_deref(),
            Some("opencode --continue")
        );
        assert_eq!(
            resume_command("opencode", Some("ses_1; rm -rf /")).as_deref(),
            Some("opencode --continue")
        );
        // Agents that resume by id only when listed.
        assert_eq!(
            resume_command("claude", Some("0f6ed4b2-755b-4e18-8bd1-f60252548036")).as_deref(),
            Some("claude --continue")
        );
    }

    #[test]
    fn test_agent_session_json() {
        let session = AgentSession {
            command: "claude --model opus".into(),
            working_directory: Some(PathBuf::from("/work/project")),
            session_id: None,
        };
        let json = session.to_json().expect("serializes");
        assert!(!json.contains("session_id"), "{json}");
        assert_eq!(AgentSession::from_json(&json), Some(session));
        assert_eq!(AgentSession::from_json("not json"), None);
        let session = AgentSession {
            command: "opencode".into(),
            working_directory: None,
            session_id: Some("ses_4f2a1cB9".into()),
        };
        let json = session.to_json().expect("serializes");
        assert_eq!(AgentSession::from_json(&json), Some(session));
        // Saved before session ids were.
        assert_eq!(
            AgentSession::from_json(r#"{"command":"opencode","working_directory":null}"#)
                .and_then(|session| session.session_id),
            None
        );
    }

    #[gpui::test]
    async fn test_saved_agent_session_round_trip() {
        let db =
            TerminalDb::open_test_db_with_workspaces("test_saved_agent_session_round_trip").await;
        // The terminal's workspace isn't what's tested.
        db.write(|connection| connection.exec("PRAGMA foreign_keys = OFF")?())
            .await
            .expect("turns off foreign keys");
        let workspace_id = workspace::WorkspaceId::from_i64(1);
        let item_id = 7;
        assert_eq!(saved_agent_session(&db, item_id, workspace_id), None);

        let session = AgentSession {
            command: "claude".into(),
            working_directory: Some(PathBuf::from("/work/project")),
            session_id: None,
        };
        db.save_agent_session(item_id, workspace_id, session.to_json())
            .await
            .expect("saves");
        assert_eq!(
            saved_agent_session(&db, item_id, workspace_id),
            Some(session.clone())
        );

        // Saving the terminal's other state keeps it.
        db.save_working_directory(item_id, workspace_id, PathBuf::from("/work"))
            .await
            .expect("saves");
        db.save_custom_title(item_id, workspace_id, Some("agent".into()))
            .await
            .expect("saves");
        assert_eq!(
            saved_agent_session(&db, item_id, workspace_id),
            Some(session)
        );
        // A terminal saved before agents were has nothing to offer.
        db.save_custom_title(8, workspace_id, None)
            .await
            .expect("saves");
        assert_eq!(saved_agent_session(&db, 8, workspace_id), None);

        db.save_agent_session(item_id, workspace_id, None)
            .await
            .expect("clears");
        assert_eq!(saved_agent_session(&db, item_id, workspace_id), None);
    }

    #[test]
    fn test_agent_display_name() {
        assert_eq!(agent_display_name("claude --continue"), "claude");
        assert_eq!(agent_display_name(r#""C:\tools\codex.exe" exec"#), "codex");
    }
}
