# Solo and Orca

[← Comparison](index.md)

[Orca](https://github.com/stablyai/orca) is the most widely used open-source ADE (agent development environment), built by a YC company and updated quickly.

> As of early November 2026; Orca's own site is the reference.

## Where Orca is ahead

- **More ground covered.** A phone app, and hooks installed for twenty-odd agents. Solo's phone app is still in development; 9 agents report their status to Solo through hooks or plugins, 2 more can be turned on in settings, and the rest are read from the screen.
- **Several platforms.** Solo is Windows only and installs only from the Microsoft Store.
- **More mature.** More users, so problems are found and fixed sooner.

## Where Solo differs

- **The editor.** Orca uses Monaco, and much of its most-upvoted wishlist is about editing: diffs across several repositories at once (#1099), "find references" returning nothing (#961), embedding VS Code (#7754). Solo is Zed: language servers, go to definition, references, the debugger and multiple windows are there, and agent tasks can span repositories.
- **Per-turn review.** Orca has line comments and a PR flow; Solo also records each turn of an agent, so you can keep or reject hunks, roll back to before a turn, and see how many errors the turn added.
- **Agents run in your own terminal**, with the Claude Code, Codex or other CLIs and subscriptions you already have; by default Solo adds hooks for each session only and doesn't change your configuration.
- **Both have a built-in browser.** In Solo's you can also pick page elements and send comments to the agent; with a setting turned on, Claude Code and Codex can open pages, click, fill forms and take screenshots in it.

## Which to pick

For a phone app, or macOS or Linux: Orca. If most of your time is reading code and reviewing what agents changed, in projects that need full language servers: try Solo. They can be used side by side too; the agents aren't tied to either.
