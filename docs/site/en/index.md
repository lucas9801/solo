# Solo

**Agents write the code. You read it, judge it and direct them.**

Solo is an editor for working with AI agents, built on [Zed](https://github.com/zed-industries/zed). The terminal fills the window, and agents like Claude Code and Codex run in it as they always do, with your own command-line tools and subscriptions. When you need to look at code, a full editor opens on the right.

It's made for people who still read the code and the documents themselves: after each turn of an agent, you see what it changed and whether it broke anything, keep or undo it, and send it your comments.

[Download (Windows)](https://github.com/lucas9801/solo/releases) · [User guide](../../guide/en.md) · [Compared with other tools](compare/index.md) · [中文](../zh/index.md)

---

## Read: what the agent changed, at a glance

- **Per-turn review.** When an agent finishes a turn, its terminal tab shows "N files changed". Open it to see everything that turn changed, keep or reject each hunk, comment on lines and send all the comments back at once. [More →](features/review.md)
- **Diagnostics per turn.** After a turn, Solo has the language servers check the changed files and tells you how many errors and warnings the turn **added**, and how many it fixed.
- **Navigate while reviewing.** The review is an editor: go to definition, find references and search all work, and each hunk is labeled with the function it's in.
- **Documents too.** Plans and designs the agent writes open in a rendered preview (Mermaid diagrams included). Select text, comment, and send the comments back together; when the document changes again, Solo tells you which parts. [More →](features/documents.md)

## Judge: keep or undo, safely

- Snapshots live in Solo's own folder; your branch, index and stash are never touched.
- **Roll back to before any turn**, and undo the rollback if you change your mind.
- Works with more than twenty agents: Claude Code, Codex and Copilot CLI report through hooks, the others are recognized from their screen.

## Direct: several agents at once

- **Agent tasks:** each task gets its own git worktree and branch, so agents don't step on each other; merge it, open a pull request or discard it when done. A task can span several repositories in one workspace. [More →](features/parallel.md)
- **See who's doing what:** tabs show whether an agent is working, waiting for you or failed; the workspace tabs in the title bar count each project's agents, including those in the background.
- **Agents can drive Solo:** an agent in a Solo terminal can list, wait for and message the others, or start a new task, with `solo-cli agent`.

## A good terminal, too

Command blocks, an input editor at the bottom, Warp-style completions, and a failed command's errors sent to an agent in one click. [More →](features/terminal.md)

---

## Where it stands

- **Windows only**, not yet code-signed: Windows may say it protected your PC on first run; choose "More info → Run anyway".
- **Closing Solo ends the agents in it.** After a restart a terminal offers to continue its session, but the turn that was running starts over. Keeping agents running in the background has been prototyped and is being built.
- No account, no telemetry. By default Solo only contacts GitHub to check for updates (which can be turned off); everything else happens when you use a feature. See [privacy](../../guide/en.md#9-privacy-and-network).
- Open source, GPL-3.0.
