# Solo and Herdr

[← Comparison](index.md)

[Herdr](https://github.com/herdrdev/herdr) is a terminal multiplexer made for agents: one program written in Rust that splits a terminal between several agents, with a desktop app as well.

> As of early November 2026; Herdr's own site is the reference.

**Solo builds on Herdr's work:** the screen rules Solo uses to tell what twenty-odd agents are doing come from Herdr (Apache-2.0), with its copyright and license notice kept. Thanks to Herdr's maintainers.

## Where Herdr is ahead

- **Light and fast.** One program that works in any terminal, including on remote machines over SSH.
- **Agents survive closing the window.** Its background service keeps running, and after the service restarts it restores the layout and continues sessions.
- **Interfaces:** a CLI and a socket API, plus a protocol for agents to report their own status.
- **Several platforms** (the Windows version is in beta).

## Where Solo differs

- **An editor.** Herdr has none; Solo includes the full Zed, so reading code, navigating and debugging happen in the same window.
- **Per-turn review.** Herdr relies on third-party plugins; Solo has it built in: keep or reject hunks, roll back, diagnostics per turn, document comments.
- **A graphical terminal:** command blocks, the input editor at the bottom, completions, paths that open with a click.

## Which to pick

If you live in the terminal, want something light, or work on remote machines: Herdr. If you want an editor to understand what agents changed: try Solo.
