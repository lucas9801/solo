# Solo compared with other agent tools

[← Home](../index.md)

Plenty of tools help you run several AI agents at once. Here Solo is set beside four open-source ones: [Orca](orca.md), [Paseo](paseo.md), [Lody](lody.md) and [Herdr](herdr.md). They're all active, and in many ways more mature than Solo; this page says where.

> What's said about them comes from their READMEs, documentation and source as of early November 2026. It may have changed since; their own sites are the reference. If something here is wrong, please [open an issue](https://github.com/lucas9801/solo/issues).

## In one line each

- **Orca**: the most complete desktop ADE, with a phone app and a built-in browser; its code editor is lighter.
- **Paseo**: a background daemon with many clients (desktop, phone, web, CLI), kept by one developer, very open (CLI, SDK, MCP).
- **Lody**: for teams sharing one workspace; needs an account.
- **Herdr**: a terminal multiplexer, one Rust program, light and fast, no editor.
- **Solo**: a full code editor (Zed) around a terminal-first layout, focused on **reading and reviewing** what agents change.

## Side by side

| | Solo | Orca | Paseo | Lody | Herdr |
| --- | --- | --- | --- | --- | --- |
| What it is | editor + terminal | desktop ADE | daemon + desktop, phone, web, CLI | shared team workspace | terminal multiplexer, plus a desktop app |
| Built with | Rust (Zed) | Electron | Electron + Expo | Electron | Rust |
| License | GPL-3.0 | MIT | Apache-2.0 | Apache-2.0 | Apache-2.0 |
| Code editor | full Zed: language servers, go to definition, references, debugger, multiple windows | Monaco, partial language server support | yes | file browsing, external editor for editing | none |
| Where agents run | your own terminal and CLIs | terminal | daemon; Claude through the Agent SDK | ACP | terminal |
| Reviewing changes | per-turn snapshots, keep or reject hunks, roll back, diagnostics per turn, document comments | line comments, PRs | changes view | per turn or per session, line comments | third-party plugins |
| When the window closes or updates | **agents end**; their sessions can be continued after a restart | a daemon keeps them | a daemon keeps them | a daemon keeps them | a background service keeps them |
| Interface for agents | `solo-cli agent` | CLI | CLI, SDK, MCP, skills | CLI, session tools | CLI, socket API |
| Phone | through Claude's Remote Control | yes | full app | yes | over SSH |
| Built-in browser | no | yes, pick page elements | yes | annotate elements | no |
| Platforms | **Windows only**, unsigned | several; signed on Windows | several | several | several; unsigned on Windows |
| Account | none | none | none | required | none |

## When Solo fits

- You still read the code yourself: big repositories, C++, game engines, projects that need language servers and a debugger.
- Most of your time goes into **seeing what an agent changed and deciding whether to keep it**, more than writing prompts.
- Agents often write plans and designs you need to read, comment on and have revised.
- You work on Windows.

## When something else fits better

- Following and answering agents from your phone: Paseo, Lody and Orca have real phone apps.
- Agents that must not stop when the window closes or updates: Solo can't do that yet (it's been prototyped); the other four can.
- A team sharing one workspace: Lody.
- Just a light terminal multiplexer, no editor: Herdr.
- macOS or Linux: Solo is Windows only for now.
