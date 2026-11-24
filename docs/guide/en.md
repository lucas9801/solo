# Solo User Guide

[中文](zh-CN.md)

## 1. What Solo is

Solo is a terminal-first editor forked from [Zed](https://github.com/zed-industries/zed).
The terminal fills the window, and the editor opens on the right when you need it.
It is built for running AI agents (Claude Code, Codex, ...) in the terminal: you
see what each agent is doing, review its changes turn by turn, and can run
several agents in parallel in their own git worktrees.

## 2. Installing

There are two ways to install Solo; one is enough.

**The installer on GitHub.** Download
`Solo-Setup-<version>.exe` from [GitHub Releases](https://github.com/lucas9801/solo/releases/latest) (the
`solo-remote-server-linux-*.gz` files there are the Linux server for remote
development, not installers).

- The installer is **unsigned**, so Windows' SmartScreen says "Windows
  protected your PC": click **More info**, then **Run anyway**.
- You can **choose the install folder** (by default
  `%LOCALAPPDATA%\Programs\Solo`; it installs for the current user, without
  administrator rights, so pick a folder you can write to, such as
  `D:\Apps\Solo`, not `C:\Program Files`).
- It adds a Start menu entry, an optional desktop icon, "Open with Solo" in
  Explorer's context menu (under "Show more options" on Windows 11; you can
  turn it off while installing) and `solo://` links.
- **Solo updates itself**: it downloads the next release's installer from
  GitHub and runs it silently into the same folder (section 3).
- A Solo installed with the installer of 2.4.2 or earlier is this kind: run
  the new installer over it; settings and data stay. The portable zip is no
  longer made.

**Solo Code from the Microsoft Store.**
[Download Solo Code's installer](https://get.microsoft.com/installer/download/9NBW452JVWN6): it installs Solo Code for the
current user without opening the Store app. If the link doesn't download,
install it from [Solo Code's Store page](https://apps.microsoft.com/detail/9NBW452JVWN6). Microsoft signs the
Store package; it updates through the Store and may be a version behind
GitHub. It has a Start menu entry, the `solo` command in Windows'
command line, and "Open with Solo Code" in Explorer's context menu (in
Windows 11's own menu, not only under "Show more options").

To put Solo Code on another drive, choose that drive under Settings > System >
Storage > Advanced storage settings > Where new content is saved > New apps
will save to before installing it, or move it later from Settings > Apps >
Installed apps (Solo Code's "..." menu, Move). (These menu names are as
Microsoft documents them, not tested.)

**"Open with Solo Code"** ("Open with Solo" for the GitHub installer) is on files, folders, a folder's background and
drives. A folder opens in Solo, started if it isn't running; a running Solo adds a
workspace tab for it to its window and switches to it, or switches to the
folder's tab if it has one already. A file opens in the tab whose folder holds
it, or else in the current tab, never in a new window. Uninstalling removes
the menu.

Both hold `solo.exe`, `solo-cli.exe` (the command line agents use),
`solo-agent-hook.exe` (which Claude Code runs to report its status),
`solo-update-helper.exe`, `solo-terminal-host.exe` (which runs a terminal's
shell, so it can outlive Solo restarting for an update), `OpenConsole.exe` and `conpty.dll` (the
terminal runtime), and license files.

## 3. Updating

- Solo checks for a newer version 30 seconds after it starts and every 6
  hours after that (stable builds only): Solo Code asks the Microsoft Store;
  other builds (installed with the GitHub installer, or built from source)
  ask [its GitHub releases](https://github.com/lucas9801/solo/releases).
- A newer version shows an "↑ Update to <version>" button at the right end of
  the title bar, left of the window buttons; there's no notification. Its menu
  has a one-line summary and Update and Restart, Install When Solo Quits, View
  Release Notes and Skip This Version. With agents running it says which ones
  updating now would end, and puts Install When Solo Quits first. The download
  progress, "update on quit" and "Update failed" (the menu says why, with
  Retry) show on the same button. A skipped version isn't offered again; a
  newer one brings the button back. Nothing is installed without you agreeing
  to it.
- Solo Code from the Microsoft Store updates inside Solo too, with the same
  button and without opening the Store: when the Store allows it (automatic
  app updates on in the Store, a network that isn't metered) the update is
  downloaded silently first; Update and Restart lets the Store install it and
  Windows opens Solo again. When the Store can't install silently it shows its
  small confirmation dialog.
- **About Solo** shows the version, its release date and commit (click it to
  copy), whether Solo is up to date with a **Check for Updates** button, and
  links to this guide, the release notes, the repository and the license.
  **Copy** copies the version, commit and system details for a bug report.
- Solo installed with the GitHub installer downloads the release's
  `Solo-Setup-<version>.exe` (checked against the SHA-256 GitHub reports) and,
  once Solo has quit, installs it silently into the same folder; Update and
  Restart opens Solo again afterwards. For a release without an installer
  the button's menu has only Get It from the Microsoft Store, which opens
  Solo Code's Store page, and Skip This Version.
- `"auto_update": false` turns off the automatic checks. **Check for Updates**
  in the command palette always checks.

## 4. Uninstalling and removing everything

- Solo Code: uninstall it from Settings → Apps.
- Solo installed with the GitHub installer: uninstall Solo from Settings →
  Apps.
- A Solo installed from an earlier release's zip:

  ```powershell
  powershell -ExecutionPolicy Bypass -File "$env:LOCALAPPDATA\Programs\Solo\install.ps1" -Uninstall
  ```


**Uninstalling doesn't remove your settings and data**, except the files
Windows kept in Solo Code's package folder (see section 5). To remove
everything, quit Solo and run (the first line ends terminals still running in
the background):

```powershell
Get-Process solo-terminal-host -ErrorAction SilentlyContinue | Stop-Process
Remove-Item -Recurse -Force "$env:APPDATA\Solo", "$env:LOCALAPPDATA\Solo", `
  "$env:TEMP\solo-shell-integration", "$env:TEMP\solo-update" -ErrorAction SilentlyContinue
```

Outside those folders, remove these yourself if you want them gone:

- `.solo` folders in your projects (project settings, tasks, debug scenarios);
- git worktrees and branches left by agent tasks (by default in `../worktrees/`
  next to the project). Delete them from **Manage Worktrees…** in the workspace tab menu, or with
  `git worktree remove` and `git branch -D`.

Solo doesn't change your Claude Code configuration (see section 8), so there's
nothing to clean up there.

## 5. Where data and settings live

| Location | What's there |
| --- | --- |
| `%APPDATA%\Solo` | Settings: `settings.json`, `keymap.json`, `tasks.json`, `debug.json`, `AGENTS.md`, `workflows.json` (saved commands) |
| `%LOCALAPPDATA%\Solo` | Data: `logs\`, `db\` (workspace and session state), `extensions\`, `languages\` (downloaded language servers), `node\`, `external_agents\`, `terminal_sessions\` (restored terminal contents, removed after 30 days), `agent_turns\` (agent turn snapshots), `terminal-host\` (the terminal host's copy), `hang_traces\` |
| `%LOCALAPPDATA%\Solo\browser` | The built-in browser's cookies, site data and element screenshots (`picks\`, kept for a week) |
| `%LOCALAPPDATA%\Solo\solo-shell-integration` | Shell integration scripts and the files agents get for their status hooks (Claude Code's settings, Copilot's plugin, OpenCode's and Pi's plugins, ...). Solo Code keeps them in its package folder. Solo 2.5.2 and earlier used `%TEMP%\solo-shell-integration`; Solo still writes copies there while that folder exists, for terminals an older Solo started |
| `%TEMP%\solo-update` | Update installers downloaded by Solo installed with the GitHub installer, and their logs |
| `.solo\` in a project | Project settings, tasks and debug scenarios (Zed's `.zed\` is read too) |

These are separate from Zed's folders, so Solo and Zed can be installed side by side.

Solo Code from the Store: Windows may keep the new files of `%APPDATA%\Solo` and
`%LOCALAPPDATA%\Solo` in the package's own folder
(`%LOCALAPPDATA%\Packages\lucas9801.SoloCode_*\LocalCache`) instead, and
deletes that folder when Solo Code is uninstalled.

## 6. First use: running an agent in a Solo terminal

Open a project folder and run `claude` (or `codex`, `gemini`, ...) in the
terminal. Solo adds the following.

### Status in the tab
The terminal's tab shows the agent's icon in its own color (Claude Code's
orange; monochrome logos in the text color) or its initial, with its status on
its corner, like a traffic light: red with a cross, **failed**; yellow in a
breathing ring, **waiting for you**; green, **done, not seen yet** (a turn
finished while you weren't looking); a turning blue ring, **working**; a gray
hollow circle, **idle**; a gray dash, **exited**. The title is the session's
(or the agent's name), followed by a ring and percentage of how full the
context is: gray below 80%, yellow to 95%, red above. Hovering over the tab
shows a card with the status, agent, model and reasoning effort, the context,
the directory, branch and command, the tokens and time of the turn and the
session, and how fast the model answers: the time to the first token and the
output speed of the latest request and their session averages. While Solo is
in the background, you get a desktop notification when an agent needs your
input. A notification when an agent stops after working for a while is off by
default; turn it on in Settings > Terminal > Notifications
(`agent_stopped_notifications`).

#### First token, speed, turn time and effort
What each agent shows; a value Solo can't tell for sure isn't shown:

| Agent | First token and speed | Effort | From |
|---|---|---|---|
| Claude Code | most requests (not one that starts with plain text) | yes | its session transcript |
| Codex | yes | yes | its telemetry, which Solo has it send to Solo for the sessions Solo starts; the effort from its session file |
| GitHub Copilot CLI | streamed requests | no | its telemetry, which Solo has it write to a file of the terminal's |
| OpenCode | yes (not yet checked against a real OpenCode) | when its message has a variant | Solo's plugin |
| Others (Gemini CLI, aider, ...) | no | no | |

The turn's time is for every agent: from when it starts working until it
stops, less the time it waits for you. The session's is the sum of its
turns since Solo saw the agent start; a session resumed from earlier shows
none. Solo leaves Codex's and Copilot's telemetry alone when you set up
telemetry of your own. Codex's telemetry stays on this machine (Solo keeps
only these numbers and the files its patches edited, in memory); in those
terminals `OTEL_BLRP_SCHEDULE_DELAY` is 200 unless you set it, so Codex sends
it every 200 ms rather than every second.

### Pending changes
Solo takes a snapshot of the working directory when each agent turn starts
and ends (for which agents, see section 7). What the agent changed is pending
until you accept it: for each file, Solo keeps what you last accepted of it,
and the tab shows **"N files to review"** (command:
`terminal: review agent turn`). The review compares each file with what you
accepted, so changes from several turns show together, each hunk marked with
its turn, and with more than one turn you can show one turn's at a time. In
the review you can:

- **Keep** or **Reject** each hunk, or **Keep All** / **Reject All**, also
  after a later turn changed the same file. Rejecting a file the agent
  created moves it to the recycle bin; a file that was already there before
  (ignored by `.gitignore` then, say) is left alone. A file with unsaved
  edits in the editor isn't rejected;
- comment on lines and send the comments, and what you rejected, to the agent
  in one message (pasted into its prompt, not sent);
- **Turns**: **View** shows what one turn changed, read only, and **Roll
  Back** puts the work tree back to before any recorded turn;
- see what the turn did to the language servers' diagnostics: the errors and
  warnings it added in the files it changed, and the errors it fixed (click
  one to go there);
- read the code as in the editor: go to definition, find references and
  search work, and a line above each hunk names the function or type it's in.

Files the turn under way is changing are locked until it ends. Changes you
don't review are accepted when the next turn ends (not while the review is
open). What you change yourself that doesn't touch the agent's hunks, and what
you commit, isn't pending. The pending changes are kept across restarts; what
the agent changed while Solo was closed (its terminal kept running) is
pending too, marked "While Solo was closed". Closing the agent's terminal
forgets them.

Snapshots are kept in Solo's data folder. They never touch your branch, HEAD,
index, stash list or git object database, and ignored files aren't included.
Folders on network drives aren't snapshotted. Rolling back leaves files inside
linked folders (junctions, symlinks) alone, since they may be outside the
project, and stops if files changed after the confirmation was shown. If
rolling back (or undoing it) fails partway, say because another program has a
file open, the page says so and keeps **Undo Rollback**: close that program
and click it to put every file back as it was.

A turn is only the files the agent reported editing, when it reports them
(Claude Code does, through Solo's hook for its Edit and Write tools; Codex's
`apply_patch` edits come from the telemetry Solo has Codex send it, for that
session only; with `[otel]` in your own Codex config, which replaces Solo's,
`"terminal": {"codex_edit_hooks": true}` reports them through a hook instead,
which adds about a second per patch on Windows): files
you or another agent in the same repository changed meanwhile aren't in it,
and **Reject All** leaves them alone.
The page lists the other files that changed during the turn, including what
the agent's own commands changed. For an agent that doesn't report its edits,
the turn is everything that changed in the folder, and the page says some of
it may not be the agent's.

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
running `terminal.agent_task_command` (`claude` by default). **Manage
Worktrees…** in the workspace tab menu (or `agent task: manage agent tasks`)
lists the tasks: open a task's terminal, view its diff, merge it into
the branch it started from, open a pull request, or delete it. Tasks are
stored in the repository's git config, so they're still there after a restart.
Local projects only.

**New Terminal in New Worktree…** in the terminal column's **+** menu (and in
the workspace tab menu) opens the same dialog: give it a name (the branch, `agent/<name>`, shows below it)
and choose what runs in the terminal: nothing (just a shell), Claude Code or
Codex (the ones on the `PATH`; `terminal.agent_task_command` picks the
default). The menu's **Run Workflow…** (alt-shift-t) lists your workflows and
the project's tasks: a workflow fills a new terminal's input with its first
`{{argument}}` selected, without running it; a task runs as Spawn Task runs
it.

- **Several repositories:** in a workspace with folders from more than one git
  repository, New Agent Task can make the task in the others too. Each gets a
  worktree on the same new branch, side by side in one folder named for the
  task, and the agent starts in that folder, so it sees them all. Each
  repository's part is merged, opened as a pull request or deleted on its own;
  the folder goes with the last one. Setup commands from `.solo/worktrees.json`
  don't run for such tasks.
- **Trust:** the `setup` and `teardown` commands and `env` variables of a
  repository's `.solo/worktrees.json` are used only once you trust the project
  (Zed's restricted mode); until then tasks are made without them.
- **Config file:** this config used to be `.solo/tasks.json`, where Zed reads
  project tasks. An old one (a JSON object) is renamed to
  `.solo/worktrees.json` when Solo first reads it; a `.solo/tasks.json` with
  project tasks (a JSON array) is left alone.
- **Committing:** merging and pull requests take the task's commits. When it
  has uncommitted changes, **Ask the Agent to Commit** pastes a request into
  the task's agent to commit them, with a message it writes from the diff;
  press Enter in its terminal to send it.

### Agent board
The board in the left dock (`agent board: toggle focus`) lists every terminal
that runs or ran an agent and every agent task, with its status, branch, last
turn's changes, running time and last message. Agents that need input come
first; click one to go to its terminal.

A task with a pull request has one more row: its number and state, which is
one of *Checking 3/7*, *1 check failed*, *2 new comments* (both are written
together when both apply), *Ready to merge*, *Merged* or *Closed*. Solo asks
GitHub for it with `gh` (GitHub's CLI, which has to be installed and logged
in; without it nothing is checked and nothing is shown; nor is a branch that was
never pushed) while it runs: every
minute while checks run, every five minutes otherwise, and not any more once
the pull request is merged or closed. *Ready to merge* means the checks passed
(or there are none), no changes are requested and GitHub says it can be
merged. Clicking the row opens a menu: send the pull request's feedback to the
agent, open the pull request in the browser, copy its link. The feedback sent
is only what's new: failed checks (by name and commit) and comments that
weren't sent to the agent yet, by this menu, by **Send PR Feedback to Agent**
in the task list, or automatically; it's pasted into the task's agent without
pressing Enter, and the board shows a **Send to Agent** button on the row while
there's something new. Which were sent is remembered across restarts.

When a pull request gets a failed check or a new comment, or can be merged, you
get one desktop notification (not while you're looking at the task's terminal);
clicking it shows the task's terminal. Two settings under Settings > Terminal >
Pull Requests: `check_pull_requests` (default `true`; off: `gh` isn't run in
the background at all) and `auto_send_pr_feedback` (default `false`: paste new
failed checks and comments into the agent as soon as it's idle).

### Workspace tabs in the title bar
Each project folder is a workspace with its own tab in the title bar, holding
its own terminals, agents, editors and file tree. Each tab shows one status
shape for each status its agents are in: failed, waiting for you, done but not
seen yet, and working (red, yellow, green, blue, in that order), so you can see
an agent waiting in the background; its menu lists the terminals.

- `+` (or opening a folder) adds a workspace;
- `ctrl-alt-1` … `ctrl-alt-9` switch to a workspace by position;
  `ctrl-alt-pagedown` / `ctrl-alt-pageup` go to the next / previous one;
- the tab's menu (its arrow, or right click) lists its terminals to jump to;
- after a restart only the active workspace loads; the others load when you
  first switch to them;
- when a terminal's directory (its agent's, when the agent reports one) is in
  a project another tab has (its folder, or the same git repository), or in no
  folder of its tab, a bar on top of the terminal says so, with **Move to**
  *tab* or **Open *name* in a New Tab and Move There** (named after the git
  repository it's in). Moving doesn't interrupt what runs in it, agents
  included, and the tab's other terminals going there move along. **Keep It
  Here** stops it asking about that place for that terminal, also after a
  restart; the × closes the bar until Solo starts again. A terminal in no
  folder of its tab also gets a dashed mark on its tab naming the folder it's
  in, and its hover card says so under its directory. Nothing moves on its
  own;
- a terminal tab's right-click menu has **Move to a New *name* Tab** (or
  **Move to the *tab* Tab**) and **Move to Workspace ›**, to move it to any
  workspace tab of the window at any time, or to a new workspace of a folder
  you pick;
- a workspace holding unrelated projects gets a **Split Workspace** dialog
  listing the folders that stay and those that get a tab of their own; the
  terminals in none of them each get a choice of where to go (by default a
  new tab of their own). Nothing changes until you click **Split**; **Keep
  Together** stops it asking;
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
  In Solo Code from the Microsoft Store, `solo-cli` works only in Solo's
  terminals.
- **Enable Remote Control** in a Claude Code terminal's menu types
  `/remote-control` into Claude Code, so you can carry on from the Claude app
  on your phone.

### The browser
Web pages open in a tab of the editor area, next to the terminal, shown by the
Microsoft Edge WebView2 runtime that comes with Windows 11 (on a computer
without it, the tab says so and offers the download).

- **Opening pages**: `ctrl`-click a link in a terminal, the Markdown preview
  or the timeline. Where it opens follows Settings > Browser > **Open Links
  In**: local addresses (localhost, 127.0.0.1, [::1], *.localhost) in Solo and
  the rest in the system browser by default, or all in Solo, or all in the
  system browser. `ctrl-shift`-click opens a link the other way. A page that
  is already open is switched to rather than opened again. The command
  palette's **browser: open browser** opens an empty tab.
- **Toolbar**: back, forward, reload (right-click: reload without the cache),
  the address (`5173` and `localhost:5173` both mean
  `http://localhost:5173/`; only http, https and local HTML and PDF files open), the
  viewport (desktop, tablet 768 x 1024, phone 390 x 844), **Pick**, and the
  `...` menu: developer tools (`f12`), find in page (`ctrl-f`), open in the
  system browser, copy address.
- Links that open a new window open a new tab. Open tabs come back after a
  restart (only their addresses are kept).
- While a page has focus, Solo's shortcuts with `ctrl` or `alt` (and function
  keys) still work, except those pages need for text: `ctrl-a/c/v/x/z/y`,
  word moves and deletes, `ctrl-b/i/u/k`, `ctrl-enter`. `esc` and `tab` stay
  with the page.
- Cookies and site data are kept in `%LOCALAPPDATA%\Solo\browser`.

**Comments on page elements.** **Pick** outlines the element under the mouse
with its selector and size; clicking one (the page's own click handlers don't
run) opens a box for your comment. Added elements get a number on the page;
click it to change or delete the comment. `esc` stops picking. **Send to
Agent · n** pastes the comments into the prompt of the agent used last
(without sending it); the arrow next to it lets you choose the agent. Each
comment comes with the element's selector, text, HTML (cut at about 1 KB), a
few computed styles, and the path of a screenshot of the element, kept in
`%LOCALAPPDATA%\Solo\browser\picks` for a week. With no agent running, the
comments are copied to the clipboard. After a reload or a hot update the
elements are found again by their selectors.

**Letting agents use the browser.** With Settings > Browser > **Allow Agents
to Control the Browser** on (off by default; takes effect after Solo
restarts), Solo's browser opens a debugging port on 127.0.0.1 (a random one),
and agents started afterwards in Solo's terminals can open pages, read them,
click, fill in forms and take screenshots in it, while you watch. **This
includes pages you are signed in to, and any program on this computer that
finds the port can do the same**, so turn it on only while you need it.

- Claude Code gets a `solo-browser` MCP server (Playwright MCP, run with
  `npx`; Node.js is needed): the `claude` wrapper adds `--mcp-config` before
  `--settings`, which adds it to your own MCP servers without changing them.
  It needs `"claude_code_hooks"` on, like the wrapper itself.
- Codex gets the same server through a `-c mcp_servers.solo-browser=...`
  override for each session (needs `"codex_hooks"`).
- Every terminal gets `SOLO_BROWSER_CDP=http://127.0.0.1:<port>`. For other
  agents (Copilot CLI, OpenCode), add an MCP server that runs
  `npx -y @playwright/mcp@0.0.85 --cdp-endpoint $SOLO_BROWSER_CDP` in their
  own MCP configuration.
- Pages that open from a page (links that open a new window) appear as tabs
  in the editor area without taking the focus. A new tab the agent creates
  itself (Playwright's "new tab") opens in a separate window outside Solo, so
  ask the agent to use the tab that is already open. At least one browser tab
  must be open for the agent to find the browser.

### Updates, even crashes, don't stop your agents
Each terminal's shell runs in a small background process,
`solo-terminal-host.exe`. When Solo restarts for an update ("Update and
Restart", or the Store's update), or even crashes, terminals that are running
something (an agent, or a command such as a build) keep running there; when
Solo opens their workspace again, it connects to them, with the output they
printed meanwhile, and recognizes the agent again (its status light, its name
in the tab). Idle shells end with Solo as before, and closing a terminal's tab
ends what runs in it.

- Updating doesn't ask about agents in terminals. Quitting Solo yourself
  (Quit, closing its last window, Alt+F4) asks first, as closing a tab or a
  window does: "Quit and End N Programs" ends them, Cancel leaves everything
  open. Terminals left running in workspace tabs you haven't opened since
  the restart are in the list too, by their tab.
- With `"terminal": {"confirm_close": "never"}`, quitting doesn't ask and
  ends them.
- To keep them running when you quit too, as after an update:
  `"terminal": {"keep_running_on_quit": true}`.
- A terminal left running whose workspace tab you haven't opened yet waits
  for that tab and shows up in it when you open it.
- Each background process logs what Solo told it and why it ended, in
  `terminal-host-<date>.log` in Solo's logs folder (kept for a week).
- If a background process itself fails, its terminal ends; after a restart
  the terminal offers to continue the agent's session as below.

### The command input
While the shell waits at its prompt (with shell integration), commands are
typed in an editor at the bottom of the terminal.

- **Completions open as you type**, after a short pause: subcommands, options
  and their values (git branches, npm scripts, docker containers, ...) for
  about 500 well-known commands, PowerShell's own completions, commands on
  `PATH` and paths. Not on an empty line, inside quotes or in a `#` request.
  In a menu that opened by itself, **Tab** accepts and **Enter runs the
  command** as typed; in a menu opened with Tab (or `ctrl-r`), Enter accepts.
  **Esc** closes it. Results that take longer (PowerShell, branch lists)
  follow the quick ones without holding up typing. To open completions only
  with Tab: `"terminal": {"completions_as_you_type": false}`.
- **Grey suggestions** after the cursor come from commands you ran (here
  first), else from the command's spec (`git ch` suggests `eckout`); `right`
  or Tab takes one.
- **`# what you want`** and Enter asks AI for a command, which fills the input
  for you to check; nothing runs until you press Enter again. It uses your own
  Claude Code (`claude -p`, model `haiku` by default) or Codex (`codex exec`),
  whichever is installed (Claude Code first; `terminal.ai_command_agent`
  picks one), with their tools turned off: Claude Code gets no tools at all,
  Codex a read-only sandbox without its shell, so generating can't run
  commands or change files. They run in an empty folder of Solo's, so your
  project's hooks, MCP servers and CLAUDE.md/AGENTS.md don't load. **Esc**
  cancels; the time taken shows meanwhile.
- **What is sent:** the request, the shell, the system and the current folder,
  plus the last failed command and the end of its output (up to 4000
  characters; `terminal.ai_command_include_last_failure`), so
  `# fix that error` works. Select a command block and press `ctrl-i` (or
  **Ask AI for a Command About This** in its context menu) to add it too. The
  blocks being sent show as chips above the input; click their × to leave one
  out. They go to Claude Code or Codex, under your account, like anything you
  type there.
- Solo doesn't guess whether what you type is a command or a request (as
  Warp's Agent Mode does): requests start with `#`. Solo's terminals mostly
  run agents, and sending a command to an AI by mistake costs more than
  typing `#`.

### Also
- Closing a terminal or a window while an agent runs asks first
  (`terminal.confirm_close`, `"agents"` by default).
- After a restart, a terminal that was running an agent and couldn't be kept
  offers to continue its session (for example `claude --continue`).
- Closing the last terminal keeps the terminal column open, with buttons for a
  new terminal (Ctrl+Shift+`), one running Claude Code or Codex (whichever is
  on the `PATH`) and the file finder. With the column hidden (Ctrl+`) and no
  files open, the editor area shows the same shortcuts.
- The button at the right end of the terminal column's tab bar hides the
  column (Ctrl+J); its terminals and agents keep running. The terminal icon
  at the top of the activity bar on the left shows it again (or Ctrl+J), and
  while the column is hidden it carries a count of the agents that want a
  look: red failed, yellow waiting for you, green finished.

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
- **Settings.** Settings → Language Services (also "Language Services
  Settings…" at the bottom of the ⚡ menu):
  - "Enable Language Services" turns all of them off or on
    (`enable_language_server`); off, nothing starts, whatever the projects
    chose.
  - "Default Analysis Level" (`global_lsp_settings.default_analysis_mode`):
    Full, Open files only (rust-analyzer: no `cargo check` on save and no
    background indexing; clangd: no background index; other servers analyze
    everything) or Off. Used by projects where you didn't pick a level; a
    level picked in a project wins. Full by default.
  - "Ask Before Large Projects" (`global_lsp_settings.ask_before_large_projects`):
    when a heavy server (rust-analyzer, clangd, roslyn, jdtls, gopls…) starts
    in a project with more than 100,000 files and the default level is Full,
    Solo asks once how much it should analyze.
  - "Alert when memory exceeds 4 GB" (`global_lsp_settings.memory_alert`,
    `enabled` and `threshold_gb`): when the project's language servers, with
    the processes they started, stay above the threshold for about 30
    seconds, Solo shows how much each one takes, once per project until Solo
    restarts, with buttons to analyze only the open files, turn the project's
    language services off, or not alert again. It changes nothing by itself.
- **This project.** The top of the ⚡ menu shows the memory of this project's
  language servers and three buttons, Full / Open files only / Off, for all of
  them at once, including servers that start later; when they differ, none is
  highlighted and the menu says "Mixed". Below, each server has a line with
  its level and memory; in its submenu, under "In This Project", you can pick
  its own level (full analysis; "Pause background analysis (open files only)"
  for rust-analyzer and clangd; or off). Choices are remembered for the
  project (in Solo's database, not in the repository) and apply after
  restarts. Servers turned off are listed further down to turn on again.
  "Stop All Language Services" stops them until Solo restarts.
- **Turn review and pausing.** When a turn ends, Solo still asks
  rust-analyzer for one `cargo check` of the changed files, even while its
  background analysis is paused, so the turn's diagnostics stay complete; with
  the server off, Rust diagnostics aren't compared.
- **When a server fails to start**, the status bar says why when Solo
  recognizes the cause (for example a missing .NET runtime for roslyn, or a
  download that failed); click it for the fix and a download link.

### IDE tools for agents
Agents started in a Solo terminal can ask Solo's language servers about that
terminal's workspace instead of searching the text, through four read-only
tools: `workspace_symbols` (find a function or type by name),
`go_to_definition`, `find_references` (callers and uses, not just matching
text) and `diagnostics` (errors and warnings of a file, or counts for the
workspace). They use the language servers Solo already runs, so the agent
doesn't start a second rust-analyzer; asking about a file starts its server
if it isn't running yet.
- **Claude Code** gets them through the `claude` wrapper (`--mcp-config`) and
  **Codex** through the `codex` wrapper (`-c mcp_servers.solo-ide...`), as a
  server named `solo-ide`, for that session only: your own config isn't
  changed. Both need the shell integration, and Claude Code also its hooks
  (`claude_code_hooks`).
- **Other agents** that support MCP can be pointed at it: the terminal has the
  server's address in `SOLO_IDE_MCP_URL` (streamable HTTP) and the token to
  send as `Authorization: Bearer` in `SOLO_IDE_TOKEN`.
- **Proxies.** Solo adds `127.0.0.1` and `localhost` to the terminal's
  `NO_PROXY`: a proxy can't reach the server on this machine, and Codex sends
  requests through the Windows system proxy (Internet Options) even to
  `127.0.0.1`, ignoring its bypass list, unless `NO_PROXY` says otherwise.
- **Only this workspace.** The server listens on 127.0.0.1 only. Each
  workspace has its own random token, which only its terminals get, and the
  tools only take and show files of that workspace (not files outside it, not
  `.env`-like private files). They change nothing.
- **Limits.** Diagnostics can lag behind edits the agent makes on disk (for
  example rust-analyzer's `cargo check` runs when Solo saves a file), so tell
  the agent to build or test to be sure. A terminal that survived a Solo
  restart keeps the old address; open a new terminal to get the tools again.
- To turn them off: `"terminal": {"ide_tools": false}` (new terminals don't
  get them, and older ones are refused).

## 7. Supported agents

| | Claude Code | Codex, Copilot CLI, OpenCode | gemini, qwen, amp, cursor-agent and others | aider, programs in `agent_commands` |
| --- | --- | --- | --- | --- |
| Recognized as an agent (status, close confirmation, sending command blocks) | yes | yes | yes | yes |
| Where the status comes from | hooks; exact | hooks (OpenCode: a plugin); exact (Codex: after you trust them once) | its screen, with per-agent rules; may lag a moment | its output |
| Per-turn review and roll back | yes | yes | yes, turns read from the screen | no |
| Timeline, session title, token use | yes | Codex: yes (its first prompt as the title); Copilot CLI, OpenCode: no | no | no |
| Continue the session after a restart | yes (`--continue`) | yes (`codex resume --last`, `copilot --continue`, `opencode --session <id>`) | yes, with each one's resume option (not qwen or amp) | no |
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
  with `--settings %LOCALAPPDATA%\Solo\solo-shell-integration\claude-settings-<hash>.json`. Claude
  Code adds those settings to your own, so **your `~/.claude/settings.json` and
  your own hooks are not changed**. If you already have your own `claude`
  function or alias, Solo leaves it alone. Subcommands such as `claude mcp` or
  `claude update`, and commands where you pass `--settings` yourself, run as
  typed. To turn the hooks off: `"terminal": {"claude_code_hooks": false}`
  (takes effect in new terminals).
- **If those files go missing** (deleted by hand or by a cleaner), Solo writes
  them again when an agent starts, before the wrapper starts it. If it can't,
  the wrapper prints one line saying that agent runs without status reports in
  that terminal, and **Why This Agent Status?** says so too; a new terminal
  fixes it.
- **Codex hooks apply to one session only, too.** Solo's `codex` wrapper adds
  `-c hooks.<event>=...` options, which Codex adds to your own config for that
  session; **your `~/.codex/config.toml` and `hooks.json` are not changed by
  Solo**. Codex runs hooks only once you trust them, so the first time it asks:
  trust them and Codex remembers it (Codex writes that to its own config). The
  hooks run `solo-agent-hook.exe` by its full path in Solo's folder (Codex runs
  hooks with PowerShell on Windows), so Codex asks again when Solo is in
  another folder or a new Solo changes the hooks. Subcommands that don't start a session
  (`codex login`, `codex mcp`, ...) run as typed, and so does a `codex.cmd`
  without a `codex.ps1` or `codex.exe` beside it. To turn them off:
  `"terminal": {"codex_hooks": false}` (takes effect in new terminals).
- **Copilot CLI hooks apply to one session only, too.** Solo writes a small
  plugin with the hooks to `%LOCALAPPDATA%\Solo\solo-shell-integration\copilot-plugin-<hash>`,
  and its `copilot` wrapper loads it with `--plugin-dir`; **nothing is
  installed into `~/.copilot`**. Copilot runs the hooks (which run
  `solo-agent-hook.exe`) with PowerShell. Subcommands that don't start a
  session (`copilot login`, `copilot plugin`, ...) run as typed. To turn them
  off: `"terminal": {"copilot_hooks": false}` (takes effect in new terminals).
- **OpenCode's status plugin applies to one session only, too.** OpenCode
  has no command hooks, so Solo writes a small JavaScript plugin to
  `%LOCALAPPDATA%\Solo\solo-shell-integration\opencode-<hash>\opencode-plugin` (and, for OpenCode 1,
  `opencode-config\plugins`); its `opencode` wrapper loads it for that
  session through environment variables: `OPENCODE_CLI_CONFIG_CONTENT` for
  OpenCode 2 (your `cli.json` plugins plus Solo's; your `cli.json` is read,
  not changed) and `OPENCODE_CONFIG_DIR` for OpenCode 1. **Nothing is written
  to `~/.config/opencode`**. If you set either variable yourself, Solo leaves
  it alone (and that OpenCode version then shows the status read from the
  screen). The plugin runs `solo-agent-hook.exe report` when a turn starts,
  asks for a permission or an answer, and ends; it reports only the session
  the OpenCode window shows. After a restart, "Continue Session" types
  `opencode --session <id>`. OpenCode 1 installs its plugin package into
  `opencode-config` the first time. To turn it off:
  `"terminal": {"opencode_hooks": false}` (takes effect in new terminals).
- **Pi, oh-my-pi, Kilo Code CLI, Gemini CLI and Qwen Code report for one
  session only, too** (`more_agent_hooks`). Pi and oh-my-pi: the `pi` and
  `omp` wrappers add `-e <Solo's extension>` (commands like `pi install` run
  as typed). Kilo: Solo's terminals set `KILO_CONFIG_DIR` to a folder of
  Solo's with its plugin, which Kilo adds to your own plugins (if you set
  `KILO_CONFIG_DIR` yourself, Solo leaves it alone). Gemini CLI and Qwen Code:
  Solo's terminals set `GEMINI_CLI_SYSTEM_DEFAULTS_PATH` /
  `QWEN_CODE_SYSTEM_DEFAULTS_PATH` to a settings file of Solo's with status
  hooks, which they add to your own hooks; an administrator's system defaults
  file (or the one you pointed the variable at) is copied into it, so it still
  applies. Nothing is written to the agents' folders. Gemini CLI's and Qwen
  Code's hooks run Solo's hook program through their shell; this hasn't been
  checked with the real agents yet.
- **Kimi Code CLI and MastraCode only read hooks from your own config**, so
  Solo adds them only to the agents you list:
  `"terminal": {"global_agent_hooks": ["kimi", "mastracode"]}`. Solo copies the
  file to `<file>.solo-backup` before its first change
  (`~/.kimi-code/config.toml`, or `$KIMI_CODE_HOME`; `~/.mastracode/hooks.json`)
  and marks its entries; taking an agent off the list removes Solo's entries
  and nothing else. The hooks do nothing outside Solo's terminals. Take the
  agents off the list before uninstalling Solo, or the hooks keep pointing at
  its hook program.
- **Why this status?** Right-click a terminal running an agent and choose
  "Why This Agent Status?", or run `solo-cli agent why <id>` (`self` for the
  terminal you're in): it says whether the status came from the agent's hook,
  the screen rules (and which rule matched, from Solo's bundled rules or your
  own file in the `agent-detection` folder of Solo's config) or its output,
  and when the last hook report came.
- **Shell integration**: PowerShell, bash, zsh and fish started without custom
  arguments load Solo's integration script from `%LOCALAPPDATA%\Solo\solo-shell-integration`,
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
- **Updates** are installed only when you choose to: by the Microsoft Store
  for Solo Code; for Solo installed with the GitHub installer,
  `solo-update-helper.exe` runs the new version's installer silently once
  Solo has quit.
- **`solo://` links:** Solo Code's package (or the GitHub installer, in
  `HKCU\Software\Classes\solo`) registers it for `solo://` links,
  for your user only, so links such as
  `solo://settings/terminal.font_size` (jumps to that setting) or a shared
  skill link open in Solo. Uninstalling removes it. Solo doesn't
  register `zed://`, which stays Zed's, but still opens such links passed to
  it. Any web page can open such a link, so a link that opens a file,
  connects to an SSH host, or clones or opens a repository
  (`solo://file/...`, `solo://ssh/...`, `solo://git/...`) first asks you,
  naming the file, host or repository.
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
| The Microsoft Store (Solo Code) or `api.github.com` (other builds) | Solo update checks (30 s after start, then every 6 h) | `"auto_update": false` |

**Contacted only when you use a feature:**

- downloading an update: the Microsoft Store (Solo Code), or GitHub
  (`github.com` and its download servers) for Solo installed with the GitHub
  installer;
- components Solo downloads by itself, after asking (see "Asking before
  downloading" below): language servers for a trusted project (GitHub
  releases, the npm registry, `proxy.golang.org`, PyPI), `nodejs.org` if no
  suitable Node.js is installed, Prettier on a file's first format, debug
  adapters on the first debug session, GitHub Copilot's server;
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

**Asking before downloading.** Before Solo downloads a component by itself
(a language server, Node.js, Prettier, a debug adapter, GitHub Copilot,
extension updates), it asks once per component, naming where it comes from:

- **Allow (remember)** downloads it, and it updates later without asking;
- **Not now** skips it until Solo restarts;
- **Use a local path…** opens the settings file, to point Solo at a copy you
  installed (`"node": { "path": ... }`, `"lsp": { "<server>": { "binary":
  { "path": ... } } }`).

Requests that come together are asked together, with **Allow all and
remember** and **Choose…**. The trust dialog of a new project lists the
language servers Solo will ask for, with "Allow these downloads" ticked, so
trusting takes one click. Answers are kept in `downloads.allowed` in your
settings file, where you can change them. Components already downloaded count
as allowed. Prettier is installed on a file's first format, not when it
opens, and for a project you haven't trusted Solo asks even in `"auto"` mode.
npm installs Solo's packages with `--ignore-scripts` (ESLint's server and
extensions' packages need their scripts and keep them).

```json
"downloads": {
  "mode": "ask",         // "ask", "auto" (download without asking) or "never"
  "allowed": { "rust-analyzer": true, "prettier": false },
  "check_updates": true  // whether allowed components update
}
```

**Offline mode.** Click the globe at the right of the status bar (or run
`network: toggle offline mode`, or set `"network": { "offline": true }`).
Solo then only connects to this computer and the local network (`localhost`,
private and link-local addresses, names without a dot, `.local`, `.lan`), so
local models (Ollama, LM Studio), servers on your LAN and SSH keep working.
It downloads nothing and checks no versions: language servers, Node.js,
Prettier and debug adapters already downloaded are used; one that's missing
says "Offline: X not downloaded". Copilot, cloud AI providers, the ACP agent
registry, the extension store and extension updates, Solo's automatic update
checks and avatars are off. Remote development works with a remote server
already downloaded, but none is downloaded. The agents you run in the
terminal aren't affected.

**Remote development is off for now:** SSH, WSL and Docker (dev container)
remote development run a server on the remote side. Solo's releases now carry
their own for Linux (x86_64 and aarch64), but it's still being tested, so
those entries stay hidden, and using one says it isn't available yet. `ssh`
and `wsl` in the terminal work as usual. To try it, set the environment
variable `SOLO_REMOTE_DEVELOPMENT=1` and start Solo again: connecting then
downloads the remote server of your Solo version from Solo's GitHub release
(`github.com`, checked against the SHA-256 GitHub reports; like other
downloads it follows offline mode and `downloads.mode`, so it asks first
unless you allowed `remote-server`), uploads it to the
remote host, which runs it from `~/.solo_server`, and keeps its data in
`~/.local/share/solo`, apart from Zed's. Zed's servers are never contacted.
Remote macOS and Windows hosts aren't supported. Solo's own features are
written for local projects: in a remote project, expect no command blocks,
agent status hooks or built-in browser integration in its terminals, terminals
that don't outlive the window, and agent tasks that say they only work in local
projects. Opening a localhost link from a remote project says that it opens the port on this computer, not on the remote machine (not for WSL, which forwards its ports to this computer). This hasn't been tested on real hosts yet.

## 10. Troubleshooting

### Blocked by security software
**Symptoms**: a terminal doesn't open, closes right away, or never shows a
prompt; Claude Code's status never changes.

**Why**: Solo has PowerShell run scripts from its data folder, Claude Code runs
`solo-agent-hook.exe` for each event. Some security software treats these as
suspicious.

**What to do**:

1. Add these to your security software's trusted list:
   the install folder (Solo Code's is under `C:\Program Files\WindowsApps`;
   the GitHub installer's is `%LOCALAPPDATA%\Programs\Solo` by default),
   with solo.exe,
   solo-agent-hook.exe, solo-terminal-host.exe, OpenConsole.exe and
   conpty.dll; `%LOCALAPPDATA%\Solo\terminal-host` (the terminal host's
   copy); and `%LOCALAPPDATA%\Solo\solo-shell-integration`.
2. To check whether that's the cause, set
   `"terminal": {"shell_integration": false}` and open a new terminal. If it
   works now, the integration script was being blocked.
3. Report the false positive to the vendor, for example through 360's open
   platform at open.soft.360.cn (unverified). Other vendors have similar
   false-positive forms. Include the version.

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
- Changing `claude_code_hooks`, `codex_hooks`, `copilot_hooks` or `opencode_hooks` only affects
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
  Solo with the environment variable `SOLO_GENERATE_MINIDUMPS=1`; it's written
  to the logs folder (exact file name unverified).
- Report problems at [github.com/lucas9801/solo/issues](https://github.com/lucas9801/solo/issues),
  with the log attached.

## 11. Settings at a glance

Solo's own settings, under `"terminal"`:

| Setting | Default | What it does |
| --- | --- | --- |
| `shell_integration` | `true` | load the shell integration (command blocks, input editor and hooks need it) |
| `claude_code_hooks` | `true` | add status hooks to Claude Code with `--settings` |
| `codex_hooks` | `true` | add status hooks to Codex with `-c hooks...` |
| `codex_edit_hooks` | `false` | also have Codex's hooks report the files each `apply_patch` edits; only needed when your own Codex config sets up `[otel]` (about a second more per patch on Windows) |
| `copilot_hooks` | `true` | add status hooks to Copilot CLI with `--plugin-dir` |
| `opencode_hooks` | `true` | load a status plugin into OpenCode for each session |
| `ide_tools` | `true` | give Claude Code and Codex Solo's read-only IDE tools (symbols, definitions, references, diagnostics) as an MCP server |
| `more_agent_hooks` | `true` | status reporting for Pi, oh-my-pi, Kilo Code CLI, Gemini CLI and Qwen Code, for each session |
| `global_agent_hooks` | `[]` | agents (`"kimi"`, `"mastracode"`) whose own config Solo may add status hooks to |
| `keep_running_on_quit` | `false` | terminals running something keep running when you quit Solo too (they always do when it restarts for an update or crashes); false asks first and ends them |
| `agent_usage_display` | `"always"` | a running agent's context, model and effort, first token and speed, turn time and tokens in a bar at the bottom of its terminal (when narrow, whole parts go: the session's tokens first, then the turn's, then the first token and speed, then the model, leaving the context's bar and percentage) and its context on the tab; `"tooltip"` shows them only in the tab's tooltip |
| `agent_commands` | `[]` | more programs to treat as agents |
| `agent_task_command` | `"claude"` | what a new agent task runs |
| `confirm_close` | `"agents"` | when to ask before closing: `agents` / `commands` / `never` |
| `follow_working_directory` | `false` | file tree and search follow the active terminal's repository |
| `restore_contents` | `false` | show terminals' previous contents after a restart |
| `input_editor` | `true` | type commands in an editor at the bottom |
| `completions_as_you_type` | `true` | open completions in the input as you type; `false`: only with Tab |
| `ai_command_agent` | `"auto"` | which agent turns `# requests` into commands: `auto` / `claude_code` / `codex` |
| `ai_command_claude_model` | `"haiku"` | the model Claude Code generates commands with |
| `ai_command_codex_model` | `""` | the model Codex generates commands with (empty: Codex's default) |
| `ai_command_include_last_failure` | `true` | send the last failed command and the end of its output with a `#` request |
| `notify_long_commands_after_seconds` | `10` | notify when a long command finishes; 0 turns it off |
| `agent_input_notifications` | `true` | notify when an agent needs your input or rings the bell |
| `agent_stopped_notifications` | `false` | notify when an agent stops after working a while |
| `program_notifications` | `true` | let programs show desktop notifications (OSC 9/777) |

These notifications only show while Solo is in the background. All four are
also in Settings > Terminal > Notifications.

The pull requests of agent tasks (see the agent board above), in Settings >
Terminal > Pull Requests:

| Key | Default | What it does |
| --- | --- | --- |
| `check_pull_requests` | `true` | check agent tasks' pull requests with `gh` and show them on the agent board; notify about failed checks, new comments and when one can be merged (not while you're looking at the task's terminal, in the background or not) |
| `auto_send_pr_feedback` | `false` | paste new failed checks and comments of a task's pull request into its agent when the agent is idle |

Outside `"terminal"`: `"ui_language"` (`"system"`, `"en"` or `"zh"`; the
environment variable `SOLO_LANGUAGE` overrides it), `"auto_update"`,
`"disable_ai"`, `"network": { "offline": false }` (offline mode) and
`"downloads"` (`"mode": "ask"` / `"auto"` / `"never"`, `"allowed": {}`,
`"check_updates": true`; see section 9). `"disable_ai"` (and `"agent": { "enabled": false }`) turn off
only Zed's own AI features (inline assistant, edit predictions, commit message
generation); workspace tabs, terminals and the agents running in them, and
turn review are unaffected.
