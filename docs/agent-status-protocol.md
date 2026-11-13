# Reporting an agent's status to Solo

Solo shows what the AI agent in each terminal is doing on its tab, in the
agent board and on the title bar's workspace tabs: working, waiting for your
input, idle, done or failed. Claude Code, Codex and Copilot CLI report it
through hooks that Solo adds to each session it starts, and OpenCode through
a plugin Solo loads into each session (see below). Any other agent, or a script, can report it
the same way, as described here. A program that reports is shown as an agent
even if Solo doesn't know its command.

Reports also drive Solo's per-turn review: a report of `working` starts a
turn (Solo snapshots the working directory), and the next `idle` ends it
(Solo snapshots it again and offers the changes for review).

## Where

Solo's terminals are started with these environment variables:

| Variable | What it is |
|---|---|
| `SOLO_AGENT_HOOK` | The path of `solo-agent-hook`, which sends reports (only with an installed Solo, where it's next to `solo.exe`) |
| `SOLO_AGENT_TOKEN` | The terminal's secret, which every report must carry (`solo-agent-hook` adds it) |

If `SOLO_AGENT_HOOK` isn't set, the program isn't running in a Solo terminal
(or one started before Solo 1.28.4), and there's no one to report to.

Solo ignores reports without the terminal's `SOLO_AGENT_TOKEN` (since Solo
2.3.0): anything the terminal shows (a `cat`ed file, ssh or `curl` output)
could print the escape sequence, and a report starts a turn (Solo runs git
in the reported directory) and lets `solo-cli agent send` type into the
terminal. Programs started in the terminal inherit the variable; keep it out
of logs and output. Terminals started with custom shell arguments have no
nonce, and take no reports.

## The command

```sh
"$SOLO_AGENT_HOOK" report --state working --agent my-agent --seq 1
"$SOLO_AGENT_HOOK" report --state blocked --agent my-agent --seq 2
"$SOLO_AGENT_HOOK" report --state idle --agent my-agent --seq 3
```

```powershell
& $env:SOLO_AGENT_HOOK report --state working --agent my-agent --seq 1
```

| Option | Meaning |
|---|---|
| `--state working` | A turn started: the agent is working on a prompt. |
| `--state blocked` | The agent waits for the user: a permission, a question, a plan to approve. |
| `--state idle` | The turn ended; the agent waits for the next prompt. |
| `--state start` | A session started or resumed (optional). |
| `--agent <name>` | The agent's name, shown on the tab: up to 40 letters, digits, spaces, `-`, `_` or `.`. Optional for agents Solo knows by their command. |
| `--session <id>` | The agent's session id (optional): a UUID, or another id of up to 128 letters, digits, `-` or `_` (such as OpenCode's `ses_...`). Solo finds Claude Code's and Codex's transcripts by a UUID, and resumes OpenCode's session by its id after a restart. |
| `--seq <n>` | A number that grows with each report. Optional: by default it's the time in milliseconds. |
| `--directory <path>` | Where the agent works, when that isn't where its shell is (say, a session resumed from another folder). Optional. Solo snapshots, reviews and rolls back turns there. |

Reports may arrive out of order (on Windows the command returns before it
has written its report, and hooks often run in the background), so Solo
ignores a report whose `--seq` is lower than the last one's. Give every
report a `--seq` if you send them in quick succession.

The command writes to the terminal it runs in, so run it from the agent's
own process tree (a hook of the agent, or the script itself), not from
another terminal. It exits with 2 and prints its usage on bad arguments.

## The escape sequence

The command writes an OSC escape sequence to its terminal. Programs can write
it themselves instead:

```
ESC ] 6973 ; agent ; nonce=<nonce> ; <kind> ; <order> ; <session id> ; <agent> ; <directory> BEL
```

- `<nonce>`: the value of `SOLO_AGENT_TOKEN`. Reports without it, or with
  another terminal's, are ignored.
- `<kind>`: `prompt` (or `working`), `input` (or `blocked`), `stop`,
  `idle` (still waiting after a stop), `start`.
- `<order>`: the `--seq` above, decimal; may be empty.
- `<session id>`: a UUID such as `0f6ed4b2-755b-4e18-8bd1-f60252548036`, or
  another id of up to 128 letters, digits, `-` or `_`; may be empty.
- `<agent>`: the agent's name, as for `--agent`; may be left out, or empty
  before a directory.
- `<directory>`: where the agent works, as an absolute path in UTF-8 with
  every byte that isn't printable ASCII, and `;` and `%`, written as `%XX`;
  may be left out. Solo uses it instead of the shell's directory. A network
  path (`\\server\share`) is ignored, and Solo doesn't snapshot folders on
  network drives.

Fields at the end can be left out along with their `;`. For example, with
`printf`:

```sh
printf '\033]6973;agent;nonce=%s;working;1;;my-agent\a' "$SOLO_AGENT_TOKEN"
```

Solo's own hook for Claude Code, Codex and Copilot CLI sends
`ESC ] 6973 ; agent ; nonce=<nonce> ; <kind> ; <ms since the epoch> ; <session id> ; <agent> ; <directory> BEL`,
with the directory from the hook's `cwd`, which is the same format.

## OpenCode

OpenCode has no command hooks, only JavaScript plugins. In a Solo terminal,
the `opencode` wrapper loads Solo's plugin for that session (nothing goes into
OpenCode's config), and the plugin runs `solo-agent-hook report ... --agent
opencode --session <its session id> --directory <where it works>`:

| OpenCode event | Report |
|---|---|
| `session.execution.started` (OpenCode 2); `session.status` busy or retry (OpenCode 1), once per turn | `working` |
| `permission.asked` (`permission.updated` in older OpenCode 1), `question.asked`, `form.created`, also from a subagent's session | `blocked` |
| `session.execution.succeeded`, `failed` or `interrupted` (OpenCode 2); `session.status` idle, `session.idle`, `session.error` (OpenCode 1), once per turn | `idle` |
| `session.created` of a top-level session (OpenCode 1); the OpenCode 2 window switching to a session | `start` (and `working` if that session is busy) |

- **OpenCode 2** runs one background server for all its clients, so the
  plugin is a TUI plugin (it runs in the terminal, in `opencode`'s process)
  and reports only the session that window shows. It's loaded through
  `OPENCODE_CLI_CONFIG_CONTENT`, whose `plugins` list replaces the one in
  `cli.json`, so Solo puts the list from your `cli.json` first
  (<https://opencode.ai/docs/cli/config/#inline-config>,
  <https://opencode.ai/docs/build/plugins/cli/#events>).
- **OpenCode 1** runs its server in `opencode`'s process; the plugin is a
  server plugin loaded from the extra config folder in `OPENCODE_CONFIG_DIR`
  (<https://docs.opencode.ai/docs/config/#custom-directory>,
  <https://docs.opencode.ai/docs/plugins/#events>).
- Either variable set by you wins, and Solo's plugin isn't loaded for that
  version.
- Events of subagents' sessions don't start or end the turn; their
  permission requests do report `blocked`.

## What Solo shows

| Last report | Tab |
|---|---|
| `working` | Working (a spinner) |
| `blocked` | Needs your input; a desktop notification if Solo is in the background |
| `idle` while you look at the terminal | Idle |
| `idle` while you don't | Done, until you look at the terminal; then Idle. A desktop notification if Solo is in the background and the turn took a while |
| the program exits | Done, or Failed with a non-zero exit code |

After a report, typing in the terminal counts: an agent that showed a
question and then drew nothing after you answered is taken to be working on
it while it keeps drawing, and idle once it's been quiet for a few seconds.
