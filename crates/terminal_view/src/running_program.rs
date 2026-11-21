//! SOLO: what closing a terminal would end, so that closing it (its tab, its
//! window or Solo) asks first: an AI agent, or with `terminal.confirm_close`
//! set to "commands", any command. A shell waiting at its prompt closes
//! without asking.

use std::path::Path;

use gpui::App;
use settings::Settings;
use terminal::{
    TaskStatus, Terminal,
    terminal_settings::{TerminalConfirmClose, TerminalSettings},
};
use workspace::item::RunningProgram;

use crate::{TerminalView, agent_status::AgentStatus, command_blocks::is_agent_command};

/// Longest command shown in the prompt.
const MAX_COMMAND_CHARS: usize = 40;

/// The command running in the terminal, and whether it's known to be a
/// command rather than the shell itself: without shell integration only the
/// foreground process is known, which is the shell while it waits.
pub(crate) fn running_command(terminal: &Terminal) -> Option<(Option<String>, bool)> {
    if let Some(task) = terminal.task() {
        if task.status != TaskStatus::Running {
            return None;
        }
        let command = task
            .spawned_task
            .command
            .clone()
            .unwrap_or_else(|| task.spawned_task.label.clone());
        return Some((Some(command), true));
    }
    let blocks = terminal.shell_integration().blocks();
    if blocks.is_empty() {
        return Some((terminal.foreground_process_command_name(), false));
    }
    let block = blocks
        .iter()
        .rev()
        .find(|block| block.started_at.is_some())?;
    if !block.is_running() {
        return None;
    }
    let command = block
        .command
        .clone()
        .filter(|command| !command.trim().is_empty())
        .or_else(|| terminal.foreground_process_command_name());
    Some((command, true))
}

/// The program's name, e.g. "claude" for `claude --continue`.
fn program_name(command: &str) -> String {
    let program = command
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .trim_matches(['"', '\'']);
    Path::new(program)
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| program.to_string())
}

impl TerminalView {
    pub(crate) fn program_ending_on_close(&self, cx: &App) -> Option<RunningProgram> {
        let settings = TerminalSettings::get_global(cx);
        if settings.confirm_close == TerminalConfirmClose::Never {
            return None;
        }
        let (command, is_command) = running_command(self.terminal.read(cx))?;
        if let Some(agent_command) = command
            .as_deref()
            .filter(|command| is_agent_command(command, &settings.agent_commands))
        {
            let status = self
                .agent_status()
                .filter(|(_, status)| {
                    matches!(
                        status,
                        AgentStatus::Working
                            | AgentStatus::NeedsInput
                            | AgentStatus::Idle
                            | AgentStatus::Finished
                    )
                })
                .map(|(_, status)| status.label().into());
            return Some(RunningProgram {
                name: program_name(agent_command).into(),
                status,
            });
        }
        // SOLO: an agent told apart from its command, e.g. in a terminal
        // connected again to the host an earlier Solo left running.
        if let Some((name, status)) = self
            .agent_status()
            .filter(|(_, status)| status.is_running())
        {
            return Some(RunningProgram {
                name: name.to_string().into(),
                status: Some(status.label().into()),
            });
        }
        if settings.confirm_close != TerminalConfirmClose::Commands || !is_command {
            return None;
        }
        let name = match command {
            Some(command) => util::truncate_and_trailoff(command.trim(), MAX_COMMAND_CHARS),
            None => util::i18n::tr("A command", "一个命令").to_string(),
        };
        Some(RunningProgram {
            name: name.into(),
            status: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{add_display_only_terminal, init_test_with_window};
    use gpui::{Entity, TestAppContext, UpdateGlobal, VisualTestContext};
    use settings::SettingsStore;
    use workspace::{CloseIntent, Pane, SaveIntent};

    fn set_confirm_close(confirm_close: TerminalConfirmClose, cx: &mut VisualTestContext) {
        cx.update(|_, cx| {
            SettingsStore::update_global(cx, |store, cx| {
                store.update_user_settings(cx, |settings| {
                    settings.terminal.get_or_insert_default().confirm_close = Some(confirm_close);
                });
            })
        });
    }

    fn write(terminal: &Entity<Terminal>, bytes: &[u8], cx: &mut VisualTestContext) {
        terminal.update(cx, |terminal, cx| terminal.write_raw_output(bytes, cx));
        cx.run_until_parked();
    }

    fn close_terminal(
        pane: &Entity<Pane>,
        view: &Entity<TerminalView>,
        cx: &mut VisualTestContext,
    ) -> gpui::Task<anyhow::Result<()>> {
        let id = view.entity_id();
        pane.update_in(cx, |pane, window, cx| {
            pane.close_items(window, cx, SaveIntent::Close, &|item_id| item_id == id)
        })
    }

    const PROMPT: &[u8] = b"\x1b]133;A\x07$ \x1b]133;B\x07";
    const RUN_CLAUDE: &[u8] = b"claude\r\n\x1b]633;E;claude --continue\x07\x1b]133;C\x07";
    const FINISH: &[u8] = b"\r\n\x1b]133;D;0\x07\x1b]133;A\x07$ \x1b]133;B\x07";

    #[gpui::test]
    async fn test_program_ending_on_close(cx: &mut TestAppContext) {
        let (project, _workspace, window) = init_test_with_window(cx).await;
        let (_, terminal, view) = add_display_only_terminal(&project, window, false, false, cx);
        let cx = &mut VisualTestContext::from_window(window.into(), cx);
        let program = |cx: &mut VisualTestContext| {
            view.read_with(cx, |view, cx| view.program_ending_on_close(cx))
        };

        // A shell at its prompt.
        write(&terminal, PROMPT, cx);
        assert_eq!(program(cx), None);

        // Output right after input is the program answering it, not working.
        // (An agent without screen rules, whose status comes from its output.)
        std::thread::sleep(std::time::Duration::from_millis(500));
        write(
            &terminal,
            b"aider
]633;E;aider]133;C",
            cx,
        );
        write(&terminal, b"spinner", cx);
        assert_eq!(
            program(cx),
            Some(RunningProgram {
                name: "aider".into(),
                status: Some("Working".into()),
            })
        );
        write(&terminal, b"\x07", cx);
        assert_eq!(
            program(cx).and_then(|program| program.status),
            Some("Waiting for you".into())
        );

        set_confirm_close(TerminalConfirmClose::Never, cx);
        assert_eq!(program(cx), None);
        set_confirm_close(TerminalConfirmClose::Agents, cx);

        // Done, back at the prompt.
        write(&terminal, FINISH, cx);
        assert_eq!(program(cx), None);

        // Other commands only ask when set to.
        write(
            &terminal,
            b"cargo build\r\n\x1b]633;E;cargo build\x07\x1b]133;C\x07",
            cx,
        );
        assert_eq!(program(cx), None);
        set_confirm_close(TerminalConfirmClose::Commands, cx);
        assert_eq!(
            program(cx),
            Some(RunningProgram {
                name: "cargo build".into(),
                status: None,
            })
        );
        write(&terminal, FINISH, cx);
        assert_eq!(program(cx), None);
    }

    #[gpui::test]
    async fn test_closing_running_agent_asks(cx: &mut TestAppContext) {
        let (project, workspace, window) = init_test_with_window(cx).await;
        let (pane, shell, _) = add_display_only_terminal(&project, window, false, false, cx);
        let (_, agent, agent_view) = add_display_only_terminal(&project, window, true, false, cx);
        let cx = &mut VisualTestContext::from_window(window.into(), cx);
        write(&shell, PROMPT, cx);
        write(&agent, PROMPT, cx);
        write(&agent, RUN_CLAUDE, cx);
        let item_count =
            |cx: &mut VisualTestContext| pane.read_with(cx, |pane, _| pane.items_len());
        assert_eq!(item_count(cx), 2);

        // Cancelling keeps the agent's terminal.
        let close = close_terminal(&pane, &agent_view, cx);
        cx.run_until_parked();
        let (message, _) = cx.pending_prompt().expect("closing an agent asks");
        assert_eq!(message, "claude is running. Closing will end it.");
        cx.simulate_prompt_answer("Cancel");
        close.await.unwrap();
        assert_eq!(item_count(cx), 2);

        // So does cancelling closing the window or quitting.
        for close_intent in [CloseIntent::CloseWindow, CloseIntent::Quit] {
            let prepare = workspace.update_in(cx, |workspace, window, cx| {
                workspace.prepare_to_close(close_intent, window, cx)
            });
            cx.run_until_parked();
            assert!(cx.has_pending_prompt());
            cx.simulate_prompt_answer("Cancel");
            assert!(!prepare.await.unwrap());
        }
        assert_eq!(item_count(cx), 2);

        // Closing both asks once, listing the agent.
        let close_all = pane.update_in(cx, |pane, window, cx| {
            pane.close_items(window, cx, SaveIntent::Close, &|_| true)
        });
        cx.run_until_parked();
        let (message, _) = cx.pending_prompt().expect("closing an agent asks");
        assert_eq!(message, "claude is running. Closing will end it.");
        cx.simulate_prompt_answer("Close and End claude");
        close_all.await.unwrap();
        assert!(!cx.has_pending_prompt());
        assert_eq!(item_count(cx), 0);
    }

    #[gpui::test]
    async fn test_closing_shell_does_not_ask(cx: &mut TestAppContext) {
        let (project, workspace, window) = init_test_with_window(cx).await;
        let (pane, shell, shell_view) =
            add_display_only_terminal(&project, window, false, false, cx);
        let cx = &mut VisualTestContext::from_window(window.into(), cx);
        write(&shell, PROMPT, cx);

        let prepare = workspace.update_in(cx, |workspace, window, cx| {
            workspace.prepare_to_close(CloseIntent::CloseWindow, window, cx)
        });
        cx.run_until_parked();
        assert!(!cx.has_pending_prompt());
        assert!(prepare.await.unwrap());

        let close = close_terminal(&pane, &shell_view, cx);
        cx.run_until_parked();
        assert!(!cx.has_pending_prompt());
        close.await.unwrap();
        assert_eq!(pane.read_with(cx, |pane, _| pane.items_len()), 0);
    }

    #[gpui::test]
    async fn test_closing_several_agents_asks_once(cx: &mut TestAppContext) {
        let (project, _workspace, window) = init_test_with_window(cx).await;
        let (pane, first, _) = add_display_only_terminal(&project, window, false, false, cx);
        let (_, second, _) = add_display_only_terminal(&project, window, true, false, cx);
        let cx = &mut VisualTestContext::from_window(window.into(), cx);
        for terminal in [&first, &second] {
            write(terminal, PROMPT, cx);
            write(terminal, RUN_CLAUDE, cx);
        }

        let close_all = pane.update_in(cx, |pane, window, cx| {
            pane.close_items(window, cx, SaveIntent::Close, &|_| true)
        });
        cx.run_until_parked();
        let (message, detail) = cx.pending_prompt().expect("closing agents asks");
        assert_eq!(message, "Closing will end the programs running in 2 tabs.");
        assert_eq!(detail.lines().count(), 2);
        assert!(detail.lines().all(|line| line.starts_with("claude")));
        cx.simulate_prompt_answer("Cancel");
        close_all.await.unwrap();
        assert!(!cx.has_pending_prompt());
        assert_eq!(pane.read_with(cx, |pane, _| pane.items_len()), 2);
    }

    fn program(command: &str, command_line: &str) -> terminal::ShellProgram {
        terminal::ShellProgram {
            pid: 1,
            start_time: 0,
            command: command.to_string(),
            command_line: command_line.to_string(),
        }
    }

    fn agent(view: &Entity<TerminalView>, cx: &mut VisualTestContext) -> Option<String> {
        view.update_in(cx, |view, window, cx| view.update_agent_status(window, cx));
        view.read_with(cx, |view, _| {
            view.agent_status().map(|(name, _)| name.to_string())
        })
    }

    /// SOLO: a terminal connected again to the host an earlier Solo left
    /// running gets its screen, not its shell's marks, and its foreground
    /// process is the shell: the agent found running under the shell is
    /// what runs, for its status, title and closing, until the shell's next
    /// prompt.
    #[gpui::test]
    async fn test_reconnected_agent(cx: &mut TestAppContext) {
        let (project, _workspace, window) = init_test_with_window(cx).await;
        let (_, terminal, view) = add_display_only_terminal(&project, window, false, false, cx);
        let cx = &mut VisualTestContext::from_window(window.into(), cx);
        terminal.update(cx, |terminal, _| {
            terminal.set_reconnected_programs_for_test(vec![program(
                "claude --settings x.json",
                r"C:\Users\a\.local\bin\claude.exe --settings x.json",
            )])
        });
        assert_eq!(agent(&view, cx).as_deref(), Some("claude"));
        assert!(
            terminal
                .read_with(cx, |terminal, _| terminal.title(false))
                .contains("claude --settings x.json")
        );
        let closing = view.read_with(cx, |view, cx| view.program_ending_on_close(cx));
        assert_eq!(closing.map(|program| program.name), Some("claude".into()));

        // It ended: the shell shows its prompt.
        write(&terminal, PROMPT, cx);
        assert_eq!(agent(&view, cx), None);
        assert_eq!(
            view.read_with(cx, |view, cx| view.program_ending_on_close(cx)),
            None
        );
    }

    /// SOLO: the agent saved for the terminal tells which program under the
    /// shell is the agent; else a report from the agent's hooks does.
    #[gpui::test]
    async fn test_reconnected_agent_from_its_session_or_hooks(cx: &mut TestAppContext) {
        let (project, _workspace, window) = init_test_with_window(cx).await;
        let (_, terminal, view) = add_display_only_terminal(&project, window, false, false, cx);
        let cx = &mut VisualTestContext::from_window(window.into(), cx);
        terminal.update(cx, |terminal, _| {
            terminal.set_reconnected_programs_for_test(vec![program(
                "launcher --resume",
                r"C:\tools\launcher.exe C:\Users\a\.claude\x --resume",
            )])
        });
        assert_eq!(agent(&view, cx), None);
        view.update(cx, |view, _| {
            view.reconnected_agent_hint = Some("codex resume".to_string())
        });
        assert_eq!(agent(&view, cx), None, "codex isn't what runs");
        view.update(cx, |view, _| {
            view.reconnected_agent_hint = Some("claude --continue".to_string())
        });
        assert_eq!(agent(&view, cx).as_deref(), Some("claude"));

        // No saved agent: its hooks tell.
        view.update(cx, |view, _| view.reconnected_agent_hint = None);
        assert_eq!(agent(&view, cx), None);
        view.update_in(cx, |view, window, cx| {
            view.agent_hook(
                terminal::shell_integration::AgentHook {
                    kind: terminal::shell_integration::AgentHookKind::PromptSubmitted,
                    sent_at_ms: Some(1),
                    session_id: None,
                    raw_session_id: None,
                    agent: None,
                    working_directory: None,
                },
                window,
                cx,
            )
        });
        assert_eq!(agent(&view, cx).as_deref(), Some("claude"));
        let closing = view.read_with(cx, |view, cx| view.program_ending_on_close(cx));
        assert_eq!(closing.map(|program| program.name), Some("claude".into()));
    }

    /// SOLO: the agent saved for a terminal still tells which program is
    /// the agent after any number of restarts that reconnect to its host,
    /// until what runs ends.
    #[gpui::test]
    async fn test_reconnected_agent_session_kept_across_restarts(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (project, _workspace, window) = init_test_with_window(cx).await;
        let db = cx.update(|cx| crate::persistence::TerminalDb::global(cx));
        let workspace_id = db.test_workspace().await.unwrap();
        let launcher = || {
            vec![program(
                "launcher --resume",
                r"C:\tools\launcher.exe C:\Users\a\.claude\x --resume",
            )]
        };
        let saved = crate::agent_resume::AgentSession {
            command: "claude --continue".to_string(),
            working_directory: None,
            session_id: None,
        };

        // Restored after a first restart, from the saved terminal.
        let mut session = Some(saved);
        let mut item_id = 0;
        for restart in 1..=2 {
            let (_, terminal, view) =
                add_display_only_terminal(&project, window, false, false, cx);
            let cx = &mut VisualTestContext::from_window(window.into(), cx);
            terminal.update(cx, |terminal, _| {
                terminal.set_reconnected_programs_for_test(launcher())
            });
            view.update(cx, |view, cx| {
                view.workspace_id = Some(workspace_id);
                view.keep_reconnected_agent_session(
                    session.take().expect("no agent saved for the terminal"),
                    cx,
                );
            });
            cx.run_until_parked();
            assert_eq!(agent(&view, cx).as_deref(), Some("claude"), "restart {restart}");
            item_id = view.entity_id().as_u64();
            // What the next restart reads for this terminal.
            session = crate::agent_resume::saved_agent_session(&db, item_id, workspace_id);
            assert_eq!(
                session.as_ref().map(|session| session.command.as_str()),
                Some("claude --continue"),
                "saved again after restart {restart}"
            );
            if restart == 2 {
                // It ended: the shell shows its prompt; nothing saved.
                write(&terminal, PROMPT, cx);
                assert_eq!(agent(&view, cx), None);
                cx.run_until_parked();
            }
        }
        assert_eq!(
            crate::agent_resume::saved_agent_session(&db, item_id, workspace_id),
            None
        );
    }

    /// SOLO: a program the agent started that ends (a helper, say) leaves
    /// the agent recognized; the agent ending ends it.
    #[gpui::test]
    async fn test_reconnected_agent_outlives_a_helper(cx: &mut TestAppContext) {
        let (project, _workspace, window) = init_test_with_window(cx).await;
        let (_, terminal, view) = add_display_only_terminal(&project, window, false, false, cx);
        let cx = &mut VisualTestContext::from_window(window.into(), cx);
        let helper = program("node helper.js", r"node C:\x\helper.js");
        let claude = program("claude --continue", r"C:\bin\claude.exe --continue");
        terminal.update(cx, |terminal, _| {
            terminal.set_reconnected_programs_for_test(vec![helper, claude.clone()])
        });
        assert_eq!(agent(&view, cx).as_deref(), Some("claude"));
        terminal.update(cx, |terminal, cx| {
            terminal.set_reconnected_programs_still_running_for_test(vec![claude], cx)
        });
        assert_eq!(agent(&view, cx).as_deref(), Some("claude"));
        terminal.update(cx, |terminal, cx| {
            terminal.set_reconnected_programs_still_running_for_test(Vec::new(), cx)
        });
        assert_eq!(agent(&view, cx), None);
    }
}
