# Before a release

Run these on the build to be released, before tagging. Write the results
(passed, or what failed) into the release PR. About 10 minutes, plus the
scenario script's run.

Use a separate data folder (`--user-data-dir`) and fake agents: never your
own Solo data, and quit your own Solo first (a stable Solo hands what it
opens to another running stable Solo).

```powershell
# Data close to a user's: project tabs, fake claude and codex, no AI model.
script\prepare-user-like-data.ps1 -Root E:\solo-test\user-like -Exe <build>\solo.exe -Fresh
# The upgrade scenario, from the previous release's program files.
script\upgrade-scenario.ps1 -OldDir <previous release> -NewDir <build> -Root E:\solo-test\upgrade
```

## The upgrade scenario

`script\upgrade-scenario.ps1` passes (exit code 0, `out\report.md`): the
previous release with three project tabs and an agent in each, then the new
build after an "update", a crash and a normal quit.

## By hand

1. **Update and restart**: with agents running in three project tabs, two of
   them not opened since Solo started, restart for an update. Each terminal
   comes back in its own tab with its agent recognized (status light, the
   agent's name as the title); no tab has an extra empty shell.
2. **Normal quit**: Quit, and closing the last window, with agents running,
   ask "Quit and End N Programs"; Cancel leaves everything open; confirming
   ends the agents (none left in Task Manager).
3. **Crash**: end solo.exe in Task Manager with agents running, start Solo:
   the agents are still there, in their tabs.
4. **Close prompts**: closing a terminal tab, a project tab and a window with
   an agent running asks first.
5. **Several project tabs**: three or more; switch between them; a terminal
   started in another project's folder shows the bar offering to move it.
6. **Terminal panel buttons after a restart**: in every terminal pane
   (including a split one), "+" offers New Terminal, New Terminal in New
   Worktree… and Run Workflow…; the split button splits; right-click shows
   the terminal's menu.
7. **Default configuration**: no AI model set up, Chinese interface: no
   inline assist button; Ctrl+I in a terminal reaches the shell; Ctrl+I on a
   selected command block asks about it.
8. **Offline mode**: turn it on; Solo starts, opens projects and terminals,
   and nothing asks the network.
9. **Built-in browser**: open a page, go back, open a link in a new tab.
10. **Turn review**: an agent's turn changes a file; the tab shows "N files
    changed"; the review lists it; undoing the turn restores the file.
