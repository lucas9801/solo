# Solo and Orca

[← Comparison](index.md)

[Orca](https://github.com/stablyai/orca) is the most widely used open-source ADE (agent development environment), built by a YC company and updated quickly.

> As of early November 2026; Orca's own site is the reference.

## Where Orca is ahead

- **Agents survive closing the window.** A background daemon holds the terminals through quitting, auto-update restarts and app crashes. Solo can't do this yet.
- **More ground covered.** A phone app, a built-in browser (pick page elements and send them to the agent), hooks installed for twenty-odd agents.
- **Several platforms, signed on Windows.** Solo is Windows only and not yet signed.
- **More mature.** More users, so problems are found and fixed sooner.

## Where Solo differs

- **The editor.** Orca uses Monaco, and much of its most-upvoted wishlist is about editing: diffs across several repositories at once (#1099), "find references" returning nothing (#961), embedding VS Code (#7754). Solo is Zed: language servers, go to definition, references, the debugger and multiple windows are there, and agent tasks can span repositories.
- **Per-turn review.** Orca has line comments and a PR flow; Solo also records each turn of an agent, so you can keep or reject hunks, roll back to before a turn, and see how many errors the turn added.
- **Agents run in your own terminal**, with the Claude Code, Codex or other CLIs and subscriptions you already have; Solo adds hooks for each session only and doesn't change your configuration.

## Which to pick

For a phone app, a built-in browser, or agents that must never be interrupted: Orca. If most of your time is reading code and reviewing what agents changed, in projects that need full language servers: try Solo. They can be used side by side too; the agents aren't tied to either.
