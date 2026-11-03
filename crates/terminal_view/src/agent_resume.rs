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

/// The command that continues the most recent session of the agent `command`
/// ran, using the same program (so a full path or a wrapper name still works).
pub(crate) fn resume_command(command: &str) -> Option<String> {
    let (program, name) = program(command)?;
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
        };
        self.agent_resume.recorded = block.started_at;
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
        self.save_agent_session(None, cx);
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
        let Some(command) = self
            .agent_resume
            .offer
            .take()
            .and_then(|session| resume_command(&session.command))
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
        let can_continue = resume_command(&session.command).is_some();
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
            resume_command("claude").as_deref(),
            Some("claude --continue")
        );
        assert_eq!(
            resume_command("claude --model opus").as_deref(),
            Some("claude --continue")
        );
        assert_eq!(
            resume_command(r"C:\tools\Claude.exe").as_deref(),
            Some(r"C:\tools\Claude.exe --continue")
        );
        assert_eq!(
            resume_command("codex").as_deref(),
            Some("codex resume --last")
        );
        assert_eq!(resume_command("gemini").as_deref(), Some("gemini --resume"));
        assert_eq!(
            resume_command("cursor-agent").as_deref(),
            Some("cursor-agent resume")
        );
        // Not confirmed in their documentation.
        assert_eq!(resume_command("qwen"), None);
        assert_eq!(resume_command("amp"), None);
        assert_eq!(resume_command("my-agent"), None);
        assert_eq!(resume_command(""), None);
    }

    #[test]
    fn test_agent_session_json() {
        let session = AgentSession {
            command: "claude --model opus".into(),
            working_directory: Some(PathBuf::from("/work/project")),
        };
        let json = session.to_json().expect("serializes");
        assert_eq!(AgentSession::from_json(&json), Some(session));
        assert_eq!(AgentSession::from_json("not json"), None);
    }

    #[gpui::test]
    async fn test_saved_agent_session_round_trip() {
        let db = TerminalDb::open_test_db_with_workspaces("test_saved_agent_session_round_trip").await;
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
