//! SOLO: selecting and copying shell integration command blocks.

use std::time::Duration;

use gpui::{ClipboardItem, Context, Entity, Window, actions};
use settings::Settings as _;
use terminal::terminal_settings::TerminalSettings;
use terminal::shell_integration::CommandBlock;
use workspace::{Toast, Workspace, notifications::NotificationId};

use crate::{TerminalView, terminal_panel::TerminalPanel};

actions!(
    terminal,
    [
        /// Selects the command block above the selected one, or the last one.
        SelectPreviousCommandBlock,
        /// Selects the command block below the selected one.
        SelectNextCommandBlock,
        /// Clears the command block selection.
        ClearCommandBlockSelection,
        /// Copies the command line of the selected (or last) command block.
        CopyCommandBlockCommand,
        /// Copies the output of the selected (or last) command block.
        CopyCommandBlockOutput,
        /// Copies the command and output of the selected (or last) command
        /// block as Markdown, ready to paste into a prompt.
        CopyCommandBlock,
        /// Runs the command of the selected command block again.
        RerunCommandBlock,
        /// Pastes the selected (or last) command block into the terminal
        /// running an AI agent, such as Claude Code.
        SendCommandBlockToAgent,
        /// Saves the command of the selected (or last) command block as a
        /// workflow.
        SaveCommandBlockAsWorkflow,
    ]
);

/// Blocks worth selecting: the ones where a command ran, not bare prompts.
fn is_selectable(block: &CommandBlock) -> bool {
    block.output_line.is_some() || block.end_line.is_some()
}

pub(crate) fn previous_block(blocks: &[CommandBlock], selected: Option<usize>) -> Option<usize> {
    let before = selected.unwrap_or(blocks.len());
    blocks[..before.min(blocks.len())]
        .iter()
        .rposition(is_selectable)
        .or(selected)
}

pub(crate) fn next_block(blocks: &[CommandBlock], selected: Option<usize>) -> Option<usize> {
    let after = selected? + 1;
    blocks
        .get(after..)?
        .iter()
        .position(is_selectable)
        .map(|ix| ix + after)
}

/// Programs recognized as AI agents to send command blocks to.
const AGENT_COMMANDS: &[&str] = &[
    "claude",
    "codex",
    "gemini",
    "aider",
    "opencode",
    "qwen",
    "amp",
    "cursor-agent",
    "copilot",
];

/// Whether `command` runs an AI agent (by its program name), one of the
/// built-in ones or of `extra` (from the `terminal.agent_commands` setting).
pub(crate) fn is_agent_command(command: &str, extra: &[String]) -> bool {
    let Some(program) = command.split_whitespace().next() else {
        return false;
    };
    let program = program.trim_matches(['"', '\'']);
    let name = std::path::Path::new(program)
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    AGENT_COMMANDS.contains(&name.as_str())
        || extra.iter().any(|agent| agent.to_lowercase() == name)
}

/// Formats a command block for pasting into an AI prompt.
pub(crate) fn command_block_markdown(
    command: &str,
    output: &str,
    exit_code: Option<i32>,
) -> String {
    let mut markdown = format!("```\n$ {command}\n");
    if !output.is_empty() {
        markdown.push_str(output);
        markdown.push('\n');
    }
    markdown.push_str("```");
    if let Some(exit_code) = exit_code.filter(|code| *code != 0) {
        markdown.push_str(&format!("\nExit code: {exit_code}"));
    }
    markdown
}

impl TerminalView {
    pub(crate) fn selected_command_block(&self) -> Option<usize> {
        self.selected_command_block
    }

    pub(crate) fn select_command_block(&mut self, ix: Option<usize>, cx: &mut Context<Self>) {
        if self.selected_command_block == ix {
            return;
        }
        self.selected_command_block = ix;
        self.terminal.update(cx, |terminal, _| match ix {
            Some(ix) => {
                if let Some(block) = terminal.shell_integration().blocks().get(ix) {
                    let line = block.prompt_line;
                    terminal.scroll_to_line(line);
                }
            }
            None => terminal.scroll_to_bottom(),
        });
        cx.notify();
    }

    fn command_running(&self, cx: &Context<Self>) -> bool {
        self.terminal
            .read(cx)
            .shell_integration()
            .blocks()
            .last()
            .is_some_and(|block| block.is_running())
    }

    /// Keeps the running command's timer up to date.
    pub(crate) fn tick_while_command_runs(&mut self, cx: &mut Context<Self>) {
        if !self.command_running(cx) {
            self.running_command_ticker = None;
            return;
        }
        if self.running_command_ticker.is_some() {
            return;
        }
        self.running_command_ticker = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let running = this
                    .update(cx, |this, cx| {
                        cx.notify();
                        this.command_running(cx)
                    })
                    .unwrap_or(false);
                if !running {
                    break;
                }
            }
        }));
    }

    /// Drops the block selection when the user goes back to typing.
    pub(crate) fn clear_command_block_selection_on_input(&mut self, cx: &mut Context<Self>) {
        if self.selected_command_block.take().is_some() {
            cx.notify();
        }
    }

    pub(crate) fn select_previous_command_block(
        &mut self,
        _: &SelectPreviousCommandBlock,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ix = previous_block(
            self.terminal.read(cx).shell_integration().blocks(),
            self.selected_command_block,
        );
        self.select_command_block(ix, cx);
    }

    pub(crate) fn select_next_command_block(
        &mut self,
        _: &SelectNextCommandBlock,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ix = next_block(
            self.terminal.read(cx).shell_integration().blocks(),
            self.selected_command_block,
        );
        self.select_command_block(ix, cx);
    }

    pub(crate) fn clear_command_block_selection(
        &mut self,
        _: &ClearCommandBlockSelection,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_command_block(None, cx);
    }

    /// The selected block, or the last block where a command ran.
    fn target_command_block(&self, cx: &Context<Self>) -> Option<usize> {
        self.selected_command_block
            .or_else(|| previous_block(self.terminal.read(cx).shell_integration().blocks(), None))
    }

    pub(crate) fn copy_command_block_command(
        &mut self,
        _: &CopyCommandBlockCommand,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ix) = self.target_command_block(cx) else {
            return;
        };
        if let Some(command) = self.terminal.read(cx).command_block_command(ix) {
            cx.write_to_clipboard(ClipboardItem::new_string(command));
        }
    }

    pub(crate) fn copy_command_block_output(
        &mut self,
        _: &CopyCommandBlockOutput,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ix) = self.target_command_block(cx) else {
            return;
        };
        if let Some(output) = self.terminal.read(cx).command_block_output(ix) {
            cx.write_to_clipboard(ClipboardItem::new_string(output));
        }
    }

    pub(crate) fn rerun_command_block(
        &mut self,
        _: &RerunCommandBlock,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // While a command runs, the input would go to it rather than the shell.
        if self.read_only || self.command_running(cx) {
            return;
        }
        let Some(ix) = self.selected_command_block else {
            return;
        };
        let Some(command) = self.terminal.read(cx).command_block_command(ix) else {
            return;
        };
        self.select_command_block(None, cx);
        self.terminal.update(cx, |terminal, _| {
            terminal.input(format!("{command}\r").into_bytes());
        });
    }

    pub(crate) fn send_command_block_to_agent(
        &mut self,
        _: &SendCommandBlockToAgent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(markdown) = self.target_command_block_markdown(cx) else {
            return;
        };
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let this = cx.entity();
        let Some(agent) = find_agent_terminal(&workspace, &this, cx) else {
            cx.write_to_clipboard(ClipboardItem::new_string(markdown));
            workspace.update(cx, |workspace, cx| {
                struct NoAgentToast;
                workspace.show_toast(
                    Toast::new(
                        NotificationId::unique::<NoAgentToast>(),
                        "No terminal is running an AI agent; copied the block instead",
                    )
                    .autohide(),
                    cx,
                );
            });
            return;
        };
        self.select_command_block(None, cx);
        agent.update(cx, |agent, cx| {
            agent
                .terminal
                .update(cx, |terminal, _| terminal.paste(&markdown));
        });
        // Switching tabs deactivates this view, which is being updated right now.
        window.defer(cx, move |window, cx| {
            activate_terminal_view(&workspace, &agent, window, cx)
        });
    }

    pub(crate) fn save_command_block_as_workflow(
        &mut self,
        _: &SaveCommandBlockAsWorkflow,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ix) = self.target_command_block(cx) else {
            return;
        };
        let Some(command) = self.terminal.read(cx).command_block_command(ix) else {
            return;
        };
        let message = match crate::input::workflows::save(command.trim()) {
            Ok(()) => "Saved as a workflow; ctrl-shift-r in the input lists them".to_string(),
            Err(error) => format!("Couldn't save the workflow: {error:#}"),
        };
        if let Some(workspace) = self.workspace.upgrade() {
            workspace.update(cx, |workspace, cx| {
                struct WorkflowSavedToast;
                workspace.show_toast(
                    Toast::new(NotificationId::unique::<WorkflowSavedToast>(), message).autohide(),
                    cx,
                );
            });
        }
    }

    fn target_command_block_markdown(&self, cx: &Context<Self>) -> Option<String> {
        let ix = self.target_command_block(cx)?;
        let terminal = self.terminal.read(cx);
        let block = terminal.shell_integration().blocks().get(ix)?;
        Some(command_block_markdown(
            &terminal.command_block_command(ix).unwrap_or_default(),
            &terminal.command_block_output(ix).unwrap_or_default(),
            block.exit_code,
        ))
    }

    pub(crate) fn copy_command_block(
        &mut self,
        _: &CopyCommandBlock,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(markdown) = self.target_command_block_markdown(cx) {
            cx.write_to_clipboard(ClipboardItem::new_string(markdown));
        }
    }
}

/// All terminal views in the workspace: the terminal panel's and the center's.
fn terminal_views(workspace: &Entity<Workspace>, cx: &gpui::App) -> Vec<Entity<TerminalView>> {
    let workspace = workspace.read(cx);
    let mut views: Vec<_> = workspace
        .panel::<TerminalPanel>(cx)
        .map(|panel| {
            panel
                .read(cx)
                .center
                .panes()
                .into_iter()
                .flat_map(|pane| {
                    pane.read(cx)
                        .items_of_type::<TerminalView>()
                        .collect::<Vec<_>>()
                })
                .collect()
        })
        .unwrap_or_default();
    views.extend(workspace.items_of_type::<TerminalView>(cx));
    views
}

/// The other terminal running an AI agent, or else the only other terminal
/// running anything.
fn find_agent_terminal(
    workspace: &Entity<Workspace>,
    this: &Entity<TerminalView>,
    cx: &gpui::App,
) -> Option<Entity<TerminalView>> {
    let extra = &TerminalSettings::get_global(cx).agent_commands;
    let running_commands: Vec<(Entity<TerminalView>, String)> = terminal_views(workspace, cx)
        .into_iter()
        .filter(|view| view != this)
        .filter_map(|view| {
            let block = view
                .read(cx)
                .terminal
                .read(cx)
                .shell_integration()
                .blocks()
                .last()
                .filter(|block| block.is_running())?
                .clone();
            Some((view, block.command.unwrap_or_default()))
        })
        .collect();
    running_commands
        .iter()
        .find(|(_, command)| is_agent_command(command, extra))
        .or_else(|| (running_commands.len() == 1).then(|| &running_commands[0]))
        .map(|(view, _)| view.clone())
}

fn activate_terminal_view(
    workspace: &Entity<Workspace>,
    view: &Entity<TerminalView>,
    window: &mut Window,
    cx: &mut gpui::App,
) {
    let panes = workspace
        .read(cx)
        .panel::<TerminalPanel>(cx)
        .map(|panel| {
            panel
                .read(cx)
                .center
                .panes()
                .into_iter()
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
        .into_iter()
        .chain(workspace.read(cx).panes().iter().cloned())
        .collect::<Vec<_>>();
    for pane in panes {
        if let Some(index) = pane.read(cx).index_for_item(view) {
            pane.update(cx, |pane, cx| {
                pane.activate_item(index, true, true, window, cx)
            });
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(prompt_line: usize, ran: bool) -> CommandBlock {
        let mut block = CommandBlock::new(prompt_line, None);
        if ran {
            block.output_line = Some(prompt_line + 1);
        }
        block
    }

    #[test]
    fn test_block_navigation() {
        // Startup prompt, two commands, current prompt.
        let blocks = vec![
            block(0, false),
            block(1, true),
            block(5, true),
            block(8, false),
        ];
        assert_eq!(previous_block(&blocks, None), Some(2));
        assert_eq!(previous_block(&blocks, Some(2)), Some(1));
        assert_eq!(previous_block(&blocks, Some(1)), Some(1));
        assert_eq!(next_block(&blocks, Some(1)), Some(2));
        assert_eq!(next_block(&blocks, Some(2)), None);
        assert_eq!(next_block(&blocks, None), None);
        assert_eq!(previous_block(&[], None), None);
    }

    #[test]
    fn test_is_agent_command() {
        assert!(is_agent_command("claude", &[]));
        assert!(is_agent_command("claude --resume", &[]));
        assert!(is_agent_command(r"C:\tools\codex.exe exec", &[]));
        assert!(!is_agent_command("cargo build", &[]));
        assert!(!is_agent_command("", &[]));
        assert!(is_agent_command("my-agent run", &["My-Agent".to_string()]));
    }

    #[test]
    fn test_command_block_markdown() {
        assert_eq!(
            command_block_markdown("dir", "a\nb", Some(0)),
            "```\n$ dir\na\nb\n```"
        );
        assert_eq!(
            command_block_markdown("cmd /c exit 5", "", Some(5)),
            "```\n$ cmd /c exit 5\n```\nExit code: 5"
        );
    }
}
