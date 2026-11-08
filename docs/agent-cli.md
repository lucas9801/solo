# Driving Solo from an agent: `solo-cli agent`

An AI agent running in a Solo terminal (Claude Code, Codex, ...) can see the
other terminals and drive the agents in them: hand a task to another agent in
its own git worktree, ask one for a second opinion, wait for it and read its
answer. This page is written for the agent; point it here, or paste it into
its instructions.

## Am I in Solo?

Solo's terminals set:

| Variable | What it is |
|---|---|
| `SOLO_CLI` | The path of `solo-cli` (only with an installed Solo) |
| `SOLO_TERMINAL_ID` | This terminal's id, as `ls` shows it |

If `SOLO_CLI` isn't set, you aren't in a Solo terminal; none of this applies.

Run the commands as `"$SOLO_CLI" agent ...` (bash, zsh) or
`& $env:SOLO_CLI agent ...` (PowerShell). Below they're written `solo-cli
agent ...`. Every command takes `--json` for output a script can parse.

## Commands

### `ls`

```
solo-cli agent ls [--json]
```

The terminals in all Solo windows: id (`*` marks yours), agent, status,
branch, directory and title, tab-separated. Statuses:

| Status | Meaning |
|---|---|
| `working` | The agent is working on a prompt. |
| `needs-input` | It waits for an answer or a permission. |
| `idle` | It waits for the next prompt. |
| `done` | It finished a turn nobody has looked at yet; it waits for the next prompt. |
| `exited`, `failed` | The agent exited (`failed`: with an error). |

### `new-task`

```
solo-cli agent new-task --prompt <text> [--agent <command>] [--base <branch>] [--name <name>] [--json]
```

Makes a git branch and worktree for a new task (from your terminal's
repository; `--base` sets the branch it starts from, by default the one
checked out), opens a terminal there, starts the agent (`--agent`, e.g.
`codex`; by default the `agent_task_command` setting, `claude`) and sends it
the prompt once it's ready. Prints the new terminal's id. The user sees the
task in Solo and can review, merge or discard it.

### `send`

```
solo-cli agent send <id> <text>...
```

Sends the text to the agent in that terminal as a prompt (pasted, then
Enter). Fails if no agent is running there, or it doesn't take pasted text.

### `wait`

```
solo-cli agent wait <id> [--until idle|needs-input|exit] [--timeout <seconds>] [--json]
```

Blocks until the agent is idle (the default), needs input, or exits, and
prints its status. Right after a `send`, an idle agent gets 10 seconds to
start working before it counts as done. Exits with 1 if the agent exited or
the terminal closed while waiting for `idle` or `needs-input`, and with 124
on timeout.

### `output`

```
solo-cli agent output <id> [--lines <n>] [--json]
```

The agent's last answer, from its transcript when Solo reads it (Claude
Code), or else the last lines of its terminal (`--lines`, 40 by default).
With `--json`, `source` says which.

### `open`, `review`

```
solo-cli agent open <path[:line[:column]]>
solo-cli agent review <id>
```

Open a file in Solo (relative paths are from your directory), or the review
of an agent's last turn: what it changed, to keep or reject each change.

## Recipes

Hand a task to Codex and wait for it:

```sh
id=$("$SOLO_CLI" agent new-task --agent codex --prompt "Add tests for the parser in src/parse.rs")
"$SOLO_CLI" agent wait "$id" --timeout 1800
"$SOLO_CLI" agent output "$id"
```

Ask an agent in another terminal for a second opinion:

```sh
"$SOLO_CLI" agent ls
"$SOLO_CLI" agent send 3f2a91c0 "Review the diff on this branch for bugs; answer in a few bullet points."
"$SOLO_CLI" agent wait 3f2a91c0 && "$SOLO_CLI" agent output 3f2a91c0
```

Tell the user where to look:

```sh
"$SOLO_CLI" agent open src/parse.rs:120
```

## Limits

- The user decides what agents may do with the terminal setting
  `agent_control`: `"full"` (the default), `"read_only"` (no `send` or
  `new-task`) or `"off"`. Solo logs every command, with the terminal it came
  from, in its log (`zed: open log`).
- Only the user's own processes on the same machine reach Solo.
- Windows only, for now.
