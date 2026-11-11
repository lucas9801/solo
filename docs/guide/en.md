# Solo User Guide

[中文](zh-CN.md)

## 1. What Solo is

Solo is a terminal-first editor forked from [Zed](https://github.com/zed-industries/zed).
The terminal fills the window, and the editor opens on the right when you need it.
It is built for running AI agents (Claude Code, Codex, ...) in the terminal: you
see what each agent is doing, review its changes turn by turn, and can run
several agents in parallel in their own git worktrees.

## 2. Installing

Both ways install Solo for the current user only (no administrator rights) into
`%LOCALAPPDATA%\Programs\Solo`.

- **Installer** `Solo-Setup-<version>.exe`: English or Chinese setup, a Start
  menu entry, an optional desktop icon (unchecked by default), and an uninstall
  entry in Settings → Apps.
- **Portable zip** `Solo-<version>-windows-x64.zip`: run `solo.exe` straight
  from the unpacked folder, or run

  ```powershell
  powershell -ExecutionPolicy Bypass -File install.ps1
  ```

  to copy it to `%LOCALAPPDATA%\Programs\Solo` with Start menu and desktop
  shortcuts.

The install folder holds `solo.exe`, `solo-agent-hook.exe` (which Claude Code
runs to report its status), `solo-update-helper.exe` (which installs updates),
`solo-terminal-host.exe` (which runs a terminal's shell, so it can outlive
Solo), `OpenConsole.exe` and `conpty.dll` (the terminal runtime), and license
files.

## 3. Updating

- Solo checks [its GitHub releases](https://github.com/lucas9801/solo/releases)
  30 seconds after it starts and every 6 hours after that (stable builds only).
- A newer version shows a notification with a one-line summary: update and
  restart, skip this version, or view the release notes. Nothing is installed
  without you agreeing to it.
- **About Solo** shows the version, its release date and commit (click it to
  copy), whether Solo is up to date with a **Check for Updates** button, and
  links to this guide, the release notes, the repository and the license.
  **Copy** copies the version, commit and system details for a bug report.
- When you update, the installer is downloaded to `%TEMP%\solo-update\`. Solo
  closes its windows the way Quit does, so it still asks about unsaved files.
  `solo-update-helper.exe` then waits for Solo to exit, runs the installer
  silently into the same folder, and Solo starts again. Logs:
  `%TEMP%\solo-update\install.log` and `update-helper.log`.
- `"auto_update": false` turns off the automatic checks. **Check for Updates**
  in the command palette always checks.

## 4. Uninstalling and removing everything

- Installer: uninstall Solo from Settings → Apps.
- Zip:

  ```powershell
  powershell -ExecutionPolicy Bypass -File "$env:LOCALAPPDATA\Programs\Solo\install.ps1" -Uninstall
  ```

**Neither one removes your settings and data.** To remove everything, quit Solo
and run (the first line ends terminals still running in the background):

```powershell
Get-Process solo-terminal-host -ErrorAction SilentlyContinue | Stop-Process
Remove-Item -Recurse -Force "$env:APPDATA\Solo", "$env:LOCALAPPDATA\Solo", `
  "$env:TEMP\solo-shell-integration", "$env:TEMP\solo-update" -ErrorAction SilentlyContinue
```

Outside those folders, remove these yourself if you want them gone:

- `.solo` folders in your projects (project settings, tasks, debug scenarios);
- git worktrees and branches left by agent tasks (by default in `../worktrees/`
  next to the project). Delete them from **Manage Agent Tasks**, or with
  `git worktree remove` and `git branch -D`.

Solo doesn't change your Claude Code configuration (see section 8), so there's
nothing to clean up there.

## 5. Where data and settings live

| Location | What's there |
| --- | --- |
| `%APPDATA%\Solo` | Settings: `settings.json`, `keymap.json`, `tasks.json`, `debug.json`, `AGENTS.md`, `workflows.json` (saved commands) |
| `%LOCALAPPDATA%\Solo` | Data: `logs\`, `db\` (workspace and session state), `extensions\`, `languages\` (downloaded language servers), `node\`, `external_agents\`, `terminal_sessions\` (restored terminal contents, removed after 30 days), `agent_turns\` (agent turn snapshots), `terminal-host\` (the terminal host's copy), `hang_traces\` |
| `%TEMP%\solo-shell-integration` | Shell integration scripts and Claude Code hook settings |
| `%TEMP%\solo-update` | Downloaded update installers and their logs |
| `.solo\` in a project | Project settings, tasks and debug scenarios (Zed's `.zed\` is read too) |

These are separate from Zed's folders, so Solo and Zed can be installed side by side.

## 6. First use: running an agent in a Solo terminal

Open a project folder and run `claude` (or `codex`, `gemini`, ...) in the
terminal. Solo adds the following.

### Status in the tab
The terminal's tab shows the agent's icon (or initial) with its status on its
corner, like a traffic light: red with a cross, **failed**; yellow in a
breathing ring, **waiting for you**; green, **done, not seen yet** (a turn
finished while you weren't looking); a turning blue ring, **working**; a gray
hollow circle, **idle**; a gray dash, **exited**. The title is the session's
(or the agent's name), followed by a ring and percentage of how full the
context is: gray below 80%, yellow to 95%, red above. Hovering over the tab
shows a card with the status, agent and model, the context, the directory,
branch and command, and the tokens of the turn and the session. While Solo is
in the background, you get a desktop notification when an agent needs you or
stops after working for a while.

### Per-turn review
Solo takes a snapshot of the working directory when each agent turn starts
and ends (for which agents, see section 7). After a turn, the tab shows
**"N files changed"** (command: `terminal: review agent turn`). In the review
you can:

- **Keep** or **Reject** each hunk, or **Keep All** / **Reject All**;
- comment on lines and send the comments to the agent;
- **Roll Back** the work tree to before any recorded turn;
- see what the turn did to the language servers' diagnostics: the errors and
  warnings it added in the files it changed, and the errors it fixed (click
  one to go there);
- read the code as in the editor: go to definition, find references and
  search work, and a line above each hunk names the function or type it's in.

Snapshots are kept in Solo's data folder. They never touch your branch, HEAD,
index, stash list or git object database, and ignored files aren't included.

When the folder can't be snapshotted (say, it isn't a git repository and has
more than 20000 files), Solo notes the files in the project that change during
the turn (only those in the agent's working directory and not ignored), and the
tab still shows **"N files changed"**: it opens a list of the files, where a
click opens one in the editor, and says why the turn has no diff or roll back.
Then only changes in folders open in the project are seen, and files other
programs change at the same time count too.

Solo doesn't open the files an agent changes while it works.

### Timeline (Claude Code, Codex)
`terminal: show agent timeline` opens the session's timeline beside the
terminal: each turn's prompt, the files read and changed, the commands run and
the answer. It's read from the agent's own records (Claude Code's
`~/.claude/projects/...`, Codex's `~/.codex/sessions/...`),
which Solo only reads. If Solo picks the wrong session, use
`terminal: choose agent session`.

### Agent tasks (git worktrees)
**New Agent Task** in the command palette asks for a task name, then creates a
branch with its own git worktree (where it goes is set by
`git.worktree_directory`, `../worktrees` by default) and opens a terminal there
running `terminal.agent_task_command` (`claude` by default). **Manage Agent
Tasks** lists the tasks: open a task's terminal, view its diff, merge it into
the branch it started from, open a pull request, or delete it. Tasks are
stored in the repository's git config, so they're still there after a restart.
Local projects only.

- **Several repositories:** in a workspace with folders from more than one git
  repository, New Agent Task can make the task in the others too. Each gets a
  worktree on the same new branch, side by side in one folder named for the
  task, and the agent starts in that folder, so it sees them all. Each
  repository's part is merged, opened as a pull request or deleted on its own;
  the folder goes with the last one. Setup commands from `.solo/tasks.json`
  don't run for such tasks.
- **Committing:** merging and pull requests take the task's commits. When it
  has uncommitted changes, **Ask the Agent to Commit** pastes a request into
  the task's agent to commit them, with a message it writes from the diff;
  press Enter in its terminal to send it.

### Agent board
The board in the left dock (`agent board: toggle focus`) lists every terminal
that runs or ran an agent and every agent task, with its status, branch, last
turn's changes, running time and last message. Agents that need input come
first; click one to go to its terminal.

### Workspace tabs in the title bar
Each project folder is a workspace with its own tab in the title bar, holding
its own terminals, agents, editors and file tree. Each tab counts its agents that
failed, wait for you, are done but not seen yet, and work (red, yellow, green,
blue, in that order), so you can see an agent waiting in the background.

- `+` (or opening a folder) adds a workspace;
- `ctrl-alt-1` … `ctrl-alt-9` switch to a workspace by position;
  `ctrl-alt-pagedown` / `ctrl-alt-pageup` go to the next / previous one;
- the tab's menu (its arrow, or right click) lists its terminals to jump to;
- after a restart only the active workspace loads; the others load when you
  first switch to them;
- when you `cd` into a project another tab has (its folder, or the same git
  repository), Solo offers to move the terminal to that tab; **Keep It Here**
  stops it asking about that tab for that terminal;
- the first time this version of Solo starts, terminals from before there
  were tabs move to the tab of their git repository, which opens in the
  background if needed. Agent task worktrees count as their repository.
  Neither happens with `follow_working_directory` on.

### Sending things to an agent
- From the editor: `ctrl-alt-k` sends the selection or the cursor position,
  `ctrl-alt-shift-k` lets you pick which terminal to send to, and
  `ctrl-alt-shift-m` sends the file's diagnostics.
- In a terminal, select a command block (`ctrl-up/down` or click its gutter)
  and press `alt-enter` to send the command and its output to an agent in
  another terminal.
- When a command fails, **Send to Agent** next to its exit code (also in the
  context menu) pastes what failed into the agent working in the same
  directory: compiler errors with their locations and failed tests with their
  assertions (cargo, tsc, gcc/clang, MSVC, pytest, go, jest), or the output's
  end otherwise.
- In a Markdown preview, select text and choose **Comment…** in the context
  menu. The comments are listed under the document; **Send to Agent** pastes
  them all, each with its lines and text, into the agent that last changed
  the document (or the last one used). When the document changes after you
  read it, a bar above it counts the added and removed lines; **Show
  Changes** marks the changed blocks, **Mark as Read** takes it as read again.
- An agent can drive the other terminals with `solo-cli agent` (list them,
  send to one, wait for it, read its answer, start a new task); see
  [agent-cli.md](../agent-cli.md). The `agent_control` setting limits this.
- **Enable Remote Control** in a Claude Code terminal's menu types
  `/remote-control` into Claude Code, so you can carry on from the Claude app
  on your phone.

### Agents keep running when Solo closes
Each terminal's shell runs in a small background process,
`solo-terminal-host.exe`. When Solo quits, restarts for an update, or even
crashes, terminals that are running something (an agent, or a command such as
a build) keep running there; when Solo opens their workspace again, it
connects to them, with the output they printed meanwhile. Idle shells end
with Solo as before, and closing a terminal's tab ends what runs in it.

- So quitting and updating don't ask about agents in terminals; closing a tab
  or a window still does. Closing Solo's last window counts as quitting.
- Each background process logs what Solo told it and why it ended, in
  `terminal-host-<date>.log` in Solo's logs folder (kept for a week).
- To turn it off: `"terminal": {"keep_running_on_quit": false}` (new
  terminals then run inside Solo and end with it).
- If a background process itself fails, its terminal ends; after a restart
  the terminal offers to continue the agent's session as below.

### Also
- Closing a terminal or a window while an agent runs asks first
  (`terminal.confirm_close`, `"agents"` by default).
- After a restart, a terminal that was running an agent and couldn't be kept
  offers to continue its session (for example `claude --continue`).
- Closing the last terminal keeps the terminal column open, with buttons for a
  new terminal (Ctrl+Shift+`), one running Claude Code or Codex (whichever is
  on the `PATH`) and the file finder. With the column hidden (Ctrl+`) and no
  files open, the editor area shows the same shortcuts.

### Language servers on large repositories
Language servers (rust-analyzer, clangd and others) analyze the project in
the background; on a large repository that can take several GB of memory.
- **Lighter defaults.** rust-analyzer's `cargo check` runs at most a quarter
  of the logical cores' worth of `rustc` at a time (at least 2); clangd
  indexes with 2 workers at low priority and keeps precompiled headers on
  disk. To undo: in your settings, set
  `"lsp": {"rust-analyzer": {"initialization_options": {"cargo": {"extraEnv": {"CARGO_BUILD_JOBS": "default"}}}}}`,
  and give `lsp.clangd.binary.arguments` your own list (`[]` for none).
- **What they cost.** The number next to the ⚡ button in the status bar is the
  memory of all language servers, with the processes they started (`cargo`
  and `rustc` under rust-analyzer). Its tooltip has one line per server, for
  example "rust-analyzer: checking · 5 compiler processes · 3.1 GB"; the
  server's submenu shows the same.
- **Per project.** In a server's submenu, under "In This Project": full
  analysis; "Pause background analysis (open files only)" (rust-analyzer: no
  `cargo check` on save and no background indexing; clangd: no background
  index) for rust-analyzer and clangd; or off. The choice is remembered for the
  project (in Solo's database, not in the repository) and applies after
  restarts. Servers turned off are listed at the bottom of the menu to turn on
  again.
- **Large projects.** When a heavy server (rust-analyzer, clangd, roslyn,
  jdtls, gopls…) starts in a project with more than 100,000 files, Solo asks
  once how much it should analyze.
- **Turn review and pausing.** When a turn ends, Solo still asks
  rust-analyzer for one `cargo check` of the changed files, even while its
  background analysis is paused, so the turn's diagnostics stay complete; with
  the server off, Rust diagnostics aren't compared.
- **When a server fails to start**, the status bar says why when Solo
  recognizes the cause (for example a missing .NET runtime for roslyn, or a
  download that failed); click it for the fix and a download link.

## 7. Supported agents

| | Claude Code | Codex, Copilot CLI | gemini, opencode, qwen, amp, cursor-agent and others | aider, programs in `agent_commands` |
| --- | --- | --- | --- | --- |
| Recognized as an agent (status, close confirmation, sending command blocks) | yes | yes | yes | yes |
| Where the status comes from | hooks; exact | hooks; exact (Codex: after you trust them once) | its screen, with per-agent rules; may lag a moment | its output |
| Per-turn review and roll back | yes | yes | yes, turns read from the screen | no |
| Timeline, session title, token use | yes | Codex: yes (its first prompt as the title); Copilot CLI: no | no | no |
| Continue the session after a restart | yes (`--continue`) | yes (`codex resume --last`, `copilot --continue`) | yes, with each one's resume option (not qwen or amp) | no |
| Agent tasks | the default command | set `agent_task_command` to it | same | same |

For agents without hooks, Solo reads the status from the terminal: a spinner
means working, about 3 seconds without output means stopped, and a bell or
notification (OSC 9/777) means it needs you.

To treat another program as an agent:

```json
{ "terminal": { "agent_commands": ["my-agent"] } }
```

## 8. What Solo does to your system

- **Claude Code hooks apply to one session only.** In a terminal with shell
  integration, Solo defines a `claude` wrapper function that starts Claude
  with `--settings %TEMP%\solo-shell-integration\claude-settings.json`. Claude
  Code adds those settings to your own, so **your `~/.claude/settings.json` and
  your own hooks are not changed**. If you already have your own `claude`
  function or alias, Solo leaves it alone. Subcommands such as `claude mcp` or
  `claude update`, and commands where you pass `--settings` yourself, run as
  typed. To turn the hooks off: `"terminal": {"claude_code_hooks": false}`
  (takes effect in new terminals).
- **Codex hooks apply to one session only, too.** Solo's `codex` wrapper adds
  `-c hooks.<event>=...` options, which Codex adds to your own config for that
  session; **your `~/.codex/config.toml` and `hooks.json` are not changed by
  Solo**. Codex runs hooks only once you trust them, so the first time it asks:
  trust them and Codex remembers it (Codex writes that to its own config). The
  hooks run `solo-agent-hook.exe`. Subcommands that don't start a session
  (`codex login`, `codex mcp`, ...) run as typed, and so does a `codex.cmd`
  without a `codex.ps1` or `codex.exe` beside it. To turn them off:
  `"terminal": {"codex_hooks": false}` (takes effect in new terminals).
- **Copilot CLI hooks apply to one session only, too.** Solo writes a small
  plugin with the hooks to `%TEMP%\solo-shell-integration\copilot-plugin`,
  and its `copilot` wrapper loads it with `--plugin-dir`; **nothing is
  installed into `~/.copilot`**. Copilot runs the hooks (which run
  `solo-agent-hook.exe`) with PowerShell. Subcommands that don't start a
  session (`copilot login`, `copilot plugin`, ...) run as typed. To turn them
  off: `"terminal": {"copilot_hooks": false}` (takes effect in new terminals).
- **Shell integration**: PowerShell, bash, zsh and fish started without custom
  arguments load Solo's integration script from `%TEMP%\solo-shell-integration`,
  after your own profile. PowerShell runs it with `-NoExit -Command`, which
  reads the script file; there's no `-ExecutionPolicy Bypass` and no
  `-EncodedCommand`. To turn it off: `"terminal": {"shell_integration": false}`.
- **The hook program** is `solo-agent-hook.exe` from the install folder. It
  reads the event from its input, writes one escape sequence to the terminal
  and exits. It doesn't use the network and doesn't write files.
- **Turn snapshots** are kept in `%LOCALAPPDATA%\Solo\agent_turns`, apart from
  your repository.
- **Agent tasks** create a branch and a worktree in your repository and record
  the task in its git config (`branch.<branch>.solotask` / `solobase`, and
  `sologroup` for a task spanning repositories). This
  only happens when you create a task.
- **Updates** are installed only when you choose to, by
  `solo-update-helper.exe` after Solo exits.
- **`solo://` links:** the installer registers Solo for `solo://` links,
  for your user only (`HKCU\Software\Classes\solo`), so a link copied from
  the settings window opens in Solo. Uninstalling removes it. Solo doesn't
  register `zed://`, which stays Zed's, but still opens such links passed to
  it.
- **Terminal hosts:** each terminal's shell runs in its own
  `solo-terminal-host.exe`, started from a copy in
  `%LOCALAPPDATA%\Solo\terminal-host` (so updates can replace Solo's files
  while one runs). It talks to Solo over named pipes only your user can open,
  and ends with its shell, with its tab, or with Solo unless something is
  running in it (see section 6).
- Solo reads Claude Code's transcripts (`~/.claude/projects`) and Codex's
  sessions (`~/.codex/sessions`) for the timeline and session details. It
  never writes to them.

## 9. Privacy and network

**Solo sends no telemetry and uploads no crash reports.** Telemetry
`diagnostics` and `metrics` are off by default. Solo's builds have no crash
upload endpoint, so no minidumps are written or uploaded. Solo doesn't sign in
to a Zed account at startup. Zed's hosted models, edit predictions,
collaboration and Zed's own auto-update are off or hidden.

**Contacted by default, without you doing anything:**

| Address | Why | How to turn it off |
| --- | --- | --- |
| `api.github.com` | Solo update checks (30 s after start, then every 6 h) | `"auto_update": false` |

**Contacted only when you use a feature:**

- downloading an update: `github.com` and GitHub's download CDN;
- language servers downloaded when you open a file in that language: GitHub
  releases, the npm registry, and `nodejs.org` if no suitable Node.js is
  installed;
- AI providers you set up yourself (Anthropic, OpenAI, Ollama, ...);
- avatars in git blame and commit details: `avatars.githubusercontent.com` and
  the GitHub API;
- browsing or installing extensions, and update checks for installed ones
  (none when no extension is installed): `api.zed.dev`, Zed's extension
  service;
- no other Zed service: help links open this guide (a copy Solo carries
  and writes to its data folder, so it opens without a connection);
  signing in to an MCP server with OAuth uses dynamic client registration
  only, never Zed's client identity (servers that only take a client metadata document can't be
  signed in to); Solo has no agent panel (Zed's own agent UI), so none of
  its downloads (e.g. a Linux build of Zed for a WSL sandbox) happen;
- looking at the ACP agent registry, or using an agent configured from it:
  `cdn.agentclientprotocol.com` and the icon URLs the registry lists;
- the agents you run in the terminal (Claude Code, Codex, ...) make their own
  connections, not through Solo.

**No remote development yet:** SSH, WSL and Docker (dev container) remote
development run a server on the remote side. Solo doesn't build its own yet,
and Zed's servers have none matching Solo, so those entries are hidden, and
using one says it isn't supported, without contacting Zed's servers. `ssh`
and `wsl` in the terminal work as usual.

## 10. Troubleshooting

### Blocked by security software
**Symptoms**: a terminal doesn't open, closes right away, or never shows a
prompt; Claude Code's status never changes; updates fail to install.

**Why**: Solo has PowerShell run scripts from `%TEMP%`, Claude Code runs
`solo-agent-hook.exe` for each event, and updates run an installer silently.
Some security software treats these as suspicious.

**What to do**:

1. Add these to your security software's trusted list:
   `%LOCALAPPDATA%\Programs\Solo` (solo.exe, solo-agent-hook.exe,
   solo-update-helper.exe, OpenConsole.exe, conpty.dll),
   `%TEMP%\solo-shell-integration` and `%TEMP%\solo-update`.
2. To check whether that's the cause, set
   `"terminal": {"shell_integration": false}` and open a new terminal. If it
   works now, the integration script was being blocked.
3. Report the false positive to the vendor, for example through 360's open
   platform at open.soft.360.cn (unverified). Other vendors have similar
   false-positive forms. Include the installer and the version.

### A terminal won't open
- Check the log (below) for `shell integration` or `failed to`.
- A shell with custom arguments isn't integrated: no command blocks, input
  editor or Claude hooks, but the terminal itself should still open.
- Without `conpty.dll` / `OpenConsole.exe` in the install folder, Solo falls back
  to Windows' own ConPTY.

### The agent status is wrong
- Use PowerShell, bash, zsh or fish without custom arguments. The `bash.exe` in
  System32 (WSL) isn't supported.
- If you have your own `claude` function or alias, Solo's wrapper isn't used,
  and the status is read from the output.
- Changing `claude_code_hooks`, `codex_hooks` or `copilot_hooks` only affects
  new terminals.
- Codex shows no status from hooks until you trust Solo's hooks in the review
  Codex shows when it starts; until then its status is read
  from the screen.
- If `solo-agent-hook.exe` is blocked or missing, Solo falls back to a
  PowerShell script, and every report is slower.
- If Solo picked the wrong session, use `terminal: choose agent session`.
- For other agents the status is read from the screen, so it can lag a second
  or two.

### Logs and crash information
- Log: `%LOCALAPPDATA%\Solo\logs\Solo.log`, and `Solo.log.old` with older
  entries (moved there when the log gets big). `solo: open log` and
  `solo: reveal log in file manager` are in the command palette.
- Hang traces: `%LOCALAPPDATA%\Solo\hang_traces\`.
- Crashes: after a crash, look in `%LOCALAPPDATA%\Solo\logs\Solo.log` (or
  `Solo.log.old`, once the log has grown big enough to be moved there). A
  crash caused by an error in Solo (a panic) is logged there, with where it
  happened and the stack backtrace, under `panicked at`; include that part
  when reporting it. Solo writes no minidumps by default. To get one, start
  Solo with the environment variable `ZED_GENERATE_MINIDUMPS=1`; it's written
  to the logs folder (exact file name unverified).
- Failed updates: `%TEMP%\solo-update\install.log` and `update-helper.log`.
- Report problems at [github.com/lucas9801/solo/issues](https://github.com/lucas9801/solo/issues),
  with the log attached.

## 11. Settings at a glance

Solo's own settings, under `"terminal"`:

| Setting | Default | What it does |
| --- | --- | --- |
| `shell_integration` | `true` | load the shell integration (command blocks, input editor and hooks need it) |
| `claude_code_hooks` | `true` | add status hooks to Claude Code with `--settings` |
| `codex_hooks` | `true` | add status hooks to Codex with `-c hooks...` |
| `copilot_hooks` | `true` | add status hooks to Copilot CLI with `--plugin-dir` |
| `keep_running_on_quit` | `true` | terminals running something keep running when Solo quits, updates or crashes, and reconnect |
| `agent_usage_display` | `"always"` | a running agent's context, model and tokens in a bar at the bottom of its terminal (when narrow, whole parts go: the session's tokens first, then the turn's, then the model, leaving the context's bar and percentage) and its context on the tab; `"tooltip"` shows them only in the tab's tooltip |
| `agent_commands` | `[]` | more programs to treat as agents |
| `agent_task_command` | `"claude"` | what a new agent task runs |
| `confirm_close` | `"agents"` | when to ask before closing: `agents` / `commands` / `never` |
| `follow_working_directory` | `false` | file tree and search follow the active terminal's repository |
| `restore_contents` | `false` | show terminals' previous contents after a restart |
| `input_editor` | `true` | type commands in an editor at the bottom |
| `notify_long_commands_after_seconds` | `10` | notify when a long command finishes; 0 turns it off |
| `program_notifications` | `true` | let programs show desktop notifications (OSC 9/777) |

Outside `"terminal"`: `"ui_language"` (`"system"`, `"en"` or `"zh"`; the
environment variable `SOLO_LANGUAGE` overrides it), `"auto_update"` and
`"disable_ai"`. `"disable_ai"` (and `"agent": { "enabled": false }`) turn off
only Zed's own AI features (inline assistant, edit predictions, commit message
generation); workspace tabs, terminals and the agents running in them, and
turn review are unaffected.
