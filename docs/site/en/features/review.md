# Per-turn review: what each turn of an agent changed

[← Home](../index.md)

Agents often change a dozen files in one go. Looking at `git diff` once they're done, you can't tell which change served which step, and undoing just part of it is hard. Solo records an agent's work by turn: you send a prompt, it works until it stops, and that's one turn.

## How it works

1. Run an agent in a Solo terminal as usual, `claude` for example.
2. When it finishes a turn, the terminal's tab shows **"N files changed"**.
3. Open it to review that turn, file by file:
   - **keep** or **reject** each hunk, or all of them;
   - comment on lines, then **send all the comments to the agent at once**;
   - **roll back to before the turn** if needed; a rollback can be undone.

## Diagnostics: did this turn break anything?

After a turn, Solo opens the changed files, waits for the language servers (rust-analyzer, clangd, TypeScript, ...) to check them, and compares the result with the start of the turn. The top of the review says:

> This turn added 2 errors and 1 warning, and fixed 3 errors

with each new problem's location and message below; click one to go there. Only the files the turn changed count, and problems that were there before don't.

Without a language server for the files, or when the check takes longer than a minute, it says "not checked" rather than pretending all is well.

## The review is an editor

Read code in the review as you would anywhere: go to definition, find references, search, and come back. A line above each hunk names the function or type it's in, so you don't scroll up for context.

## Your repository stays as it was

- Snapshots are stored in Solo's data folder. They don't change your branch, HEAD, index or stash, and don't write to the git object database.
- Rejecting a hunk restores only that hunk; other changes in the same file stay.
- Files ignored by `.gitignore` aren't snapshotted.

## Which agents

| Agent | How Solo knows when a turn starts and ends |
| --- | --- |
| Claude Code, Codex, Copilot CLI | the agent reports it through hooks Solo adds for that session; most precise. Your configuration isn't changed |
| Gemini CLI, Cursor CLI, OpenCode, Qwen Code, Amp and twenty-odd others | per-agent rules read from its screen (the rules come from the open-source Herdr); may lag a moment |
| Any other program | can report its own status with [the status protocol](../../../agent-status-protocol.md) |

Claude Code also gets a **session timeline**: each turn's prompt, the files read and changed, the commands run and the final answer.
