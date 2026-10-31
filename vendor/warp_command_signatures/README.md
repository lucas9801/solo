# warp_command_signatures

Command completion specs and generators from Warp's
[command-signatures](https://github.com/warpdotdev/command-signatures)
repository, revision in `REVISION`. MIT licensed, see `LICENSE.md` (many specs
were adapted from Fig, also MIT).

- `completion_metadata`: the spec types (`warp-completion-metadata`).
- `command_signatures`: the JSON specs and the dynamic generators
  (`warp-command-signatures`).

Changes from upstream: the PowerShell spec autogenerator (`src/bin`,
`src/powershell_autogenerator`) is left out, and dependencies are pinned to the
versions Solo already uses (rust-embed 8).

Solo's completion engine that interprets these specs lives in
`crates/terminal_view/src/input/spec.rs`.
