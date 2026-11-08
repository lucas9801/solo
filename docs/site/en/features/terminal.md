# The terminal

[← Home](../index.md)

Agents run in the terminal, and so do your own commands. Solo's terminal is built for both. It supports PowerShell, bash, zsh and fish.

## Command blocks

Each command and its output form a block: separators, exit codes and durations, failed commands in red, a live timer while running, and the command pinned to the top while you scroll through long output.

Select blocks (`ctrl-up/down`, or click the gutter) to copy the command, the output or both as Markdown, rerun them, or press `alt-enter` to send the command and its output to an agent in another terminal.

## Failed? Hand it to the agent

When a command fails, **Send to Agent** appears next to its exit code. Solo keeps only what matters: compiler errors with their locations and failed tests with their assertions (cargo, tsc, gcc/clang, MSVC, pytest, go, jest), or the end of the output otherwise, and pastes it into the agent working in the same directory.

## An input editor at the bottom

Commands are typed in a real editor at the bottom of the window: multiple cursors, undo, `shift-enter` for new lines, syntax highlighting.

- **Completions** from Warp's command signatures (about 500 commands): subcommands, options with descriptions, and argument values generated on the spot (git branches, npm scripts, docker containers, ...); PowerShell's own completions for the rest. Commands run in the current directory before come first.
- **History:** ghost-text suggestions, up/down filtered by what you typed, fuzzy search on `ctrl-r`.
- **`# what to do`:** describe it, and Solo asks Claude Code for a command for you to check and run.
- **Workflows:** saved commands with `{{arguments}}`, on `ctrl-shift-r`.

## The editor next to it

- Click a path in the terminal to open it: code in the editor, Markdown in a preview, images in the image viewer. A bare `:403` goes to the file mentioned last above it.
- Files an agent changes open in the editor as it works, so you watch it happen.
- In the editor, `ctrl-alt-k` sends the selection or the cursor position to an agent, `ctrl-alt-shift-m` the file's diagnostics.

## After a restart

Terminals come back in their places, directories and titles (with a fresh shell). A terminal that was running an agent offers to continue its session, `claude --continue` for example. To get the previous output back too, turn on `terminal.restore_contents`.
