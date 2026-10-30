//! SOLO: selecting and copying shell integration command blocks.

use std::time::Duration;

use gpui::{ClipboardItem, Context, Window, actions};
use terminal::shell_integration::CommandBlock;

use crate::TerminalView;

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

    pub(crate) fn copy_command_block(
        &mut self,
        _: &CopyCommandBlock,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ix) = self.target_command_block(cx) else {
            return;
        };
        let terminal = self.terminal.read(cx);
        let Some(block) = terminal.shell_integration().blocks().get(ix) else {
            return;
        };
        let markdown = command_block_markdown(
            &terminal.command_block_command(ix).unwrap_or_default(),
            &terminal.command_block_output(ix).unwrap_or_default(),
            block.exit_code,
        );
        cx.write_to_clipboard(ClipboardItem::new_string(markdown));
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
