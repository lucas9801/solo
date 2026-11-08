# Several agents at once

[← Home](../index.md)

## Agent tasks: a worktree each

Several agents changing code in one directory get in each other's way. Solo gives each task its own git worktree and branch:

1. Choose **New Agent Task** in the project tab's menu in the title bar (or in the command palette) and name the task.
2. Solo makes a new branch from the current one, with its worktree, opens a terminal there and starts your agent (`claude` by default).
3. When it's done, in **Manage Agent Tasks**: view the diff, **merge it into the branch it started from**, **push and open a pull request**, or delete it.

- Tasks are kept in the repository's own git config, so they're still there after a restart.
- Merging lists the commits to merge and any uncommitted changes first; on conflicts the merge is abandoned, never leaving your main work tree half merged.
- When a task has uncommitted changes, **Ask the Agent to Commit** pastes a request into the task's agent to commit them with a message it writes from the diff. Solo itself calls no model.
- Once there's a pull request, **Send PR Feedback to Agent** gathers the failed CI checks and the review comments for it. (Needs the `gh` CLI.)

### Across repositories

Frontend, backend and a shared library in separate repositories, and one change to make in all of them? Put the repositories in one workspace and tick **Also in the workspace's other repositories** when creating the task. Each repository gets a worktree on the same new branch, side by side in one folder named for the task, and the agent starts in that folder, seeing them all. Each repository's part is merged, opened as a pull request or deleted on its own.

### A repository's own task setup

A `.solo/tasks.json` in the repository sets where worktrees go, which ignored files (like `.env`) each task gets a copy of, what runs before the agent starts and before a task is deleted, and a port of its own for each task.

## Seeing who's doing what

- **Terminal tabs** show an agent's status: working, needs your input, idle, done, failed. With Solo in the background, you get a desktop notification when an agent waits for you or finishes a long stretch of work.
- **Workspace tabs in the title bar:** one per project; switching swaps the terminals, editors and file tree together. Each tab counts the agents working, waiting for you or failed in that project, so one waiting in the background is seen. A tab's menu lists the project's terminals and tasks to jump to.
- **Agent board:** the left sidebar lists every agent and task, those waiting for you first.

## Agents directing agents

An agent in a Solo terminal can see and drive the others with `solo-cli agent`:

```
solo-cli agent ls                     # every terminal and its status
solo-cli agent new-task --prompt "…"  # start a task and hand it to another agent
solo-cli agent wait <id>              # wait for it to finish
solo-cli agent output <id>            # read its last answer
solo-cli agent send <id> "…"          # send it a message
```

For example, the main agent hands off a subtask, carries on, and collects the result later; or it asks another agent for a second opinion. Give the agent [this page](../../../agent-cli.md) and it will know how. The `agent_control` setting (full, read-only or off) limits what agents can do, and every call is logged.
