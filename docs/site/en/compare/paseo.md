# Solo and Paseo

[← Comparison](index.md)

[Paseo](https://github.com/getpaseo/paseo) is a background daemon with several clients: desktop, phone, web and a CLI all connect to the same agents. It's kept by one developer.

> As of early November 2026; Paseo's own site is the reference.

## Where Paseo is ahead

- **Keep going from anywhere.** A full phone app and a web client let you follow and answer agents away from your computer.
- **Very open.** A CLI, an SDK, MCP and skills make it easy to fit into your own tooling.
- **Several platforms**, with x64 and ARM64 builds for Windows.
- No telemetry.

## Where Solo differs

- **A full editor and per-turn review.** Paseo has a changes view; Solo records each turn of an agent, so you can keep or reject hunks, roll back, and see the errors a turn added, with go to definition and find references while reviewing.
- **Agents run as their own CLIs.** According to Paseo's issue #1029 it drives Claude through the Agent SDK; Solo runs your own `claude` and other CLIs in the terminal, without the SDK.
- **Document review:** comment on documents agents write in their preview and send the comments back at once.

## Which to pick

To manage agents from your phone at any time, or to plug them into your own automation: Paseo. To read and review agents' changes closely on a Windows PC: try Solo.
