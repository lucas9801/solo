// SOLO: The extension Solo loads into Pi (`pi -e`) and oh-my-pi (`omp -e`)
// sessions started from its terminals, for that session only, so Solo knows
// what the agent is doing: working, waiting for you, or done with the turn.
// It reports with `solo-agent-hook report` (docs/agent-status-protocol.md),
// which writes to the terminal the agent runs in. Nothing is written to Pi's
// or OMP's own folders.
//
// Which events mean what follows Herdr's Pi and OMP integrations
// (https://github.com/herdrdev/herdr, src/integration/assets/pi and omp,
// Copyright Herdr contributors, Apache License 2.0); SOLO: rewritten to report
// through Solo's hook program instead of Herdr's socket, as one extension for
// both agents.
//
// Plain JavaScript in a .ts file: Pi and OMP load extensions as TypeScript,
// and Node (22.18+) can import it for the tests (solo-pi.test.mjs).

import { spawn } from "node:child_process"

/** A session id as Solo's hook takes it. */
export function isSessionId(id) {
  return typeof id === "string" && /^[A-Za-z0-9_-]{1,128}$/.test(id)
}

/** The arguments of `solo-agent-hook` for a report. */
export function reportArguments(agent, report, seq) {
  const args = ["report", "--state", report.state, "--agent", agent, "--seq", String(seq)]
  if (isSessionId(report.session)) args.push("--session", report.session)
  if (typeof report.directory === "string" && report.directory.trim() !== "") {
    args.push("--directory", report.directory)
  }
  return args
}

/** Sends reports by starting `hook` for each, in the background. */
export function hookSender(hook, agent, spawnProcess = spawn, now = Date.now) {
  let last = 0
  return (report) => {
    if (!hook || !report) return
    const seq = Math.max(now(), last + 1)
    last = seq
    try {
      const child = spawnProcess(hook, reportArguments(agent, report, seq), {
        stdio: "ignore",
        windowsHide: true,
      })
      child?.on?.("error", () => {})
      child?.unref?.()
    } catch {
      // The hook is gone: nothing to tell.
    }
  }
}

/** Pi or OMP, from the program that runs the extension. */
export function agentName(argv = process.argv) {
  return argv.some((arg) => /(^|[\\/])omp(\.exe|\.cmd|\.js)?$/i.test(arg) || /oh-my-pi/i.test(arg))
    ? "omp"
    : "pi"
}

/** Whether a session runs in the terminal: Pi's TUI mode, or OMP's UI. */
export function interactive(ctx) {
  if (typeof ctx?.mode === "string") return ctx.mode === "tui"
  return ctx?.hasUI === true
}

/**
 * What to report for the agent's events: `on(event, data, ctx)` returns the
 * reports, in order. A turn starts with `agent_start` and ends when the
 * agent settles (Pi's `agent_settled`), or after `agent_end` with nothing
 * following within `settleMs` (OMP); approvals (OMP), the `ask` tool and
 * Pi's prompts (`ui_prompt_*`) wait for the user.
 */
export function createReporter({ settleMs = 500, setTimer = setTimeout, clearTimer = clearTimeout, send } = {}) {
  let root = false
  let session
  let directory
  let active = false
  let blocked = 0
  let last
  let pendingIdle

  const report = (state) => {
    if (state === last && state !== "start") return
    last = state
    send({ state, session, directory })
  }
  const publish = () => report(blocked > 0 ? "blocked" : active ? "working" : "idle")
  const cancelIdle = () => {
    if (pendingIdle !== undefined) clearTimer(pendingIdle)
    pendingIdle = undefined
  }
  const remember = (ctx) => {
    try {
      const id = ctx?.sessionManager?.getSessionId?.()
      if (isSessionId(id)) session = id
    } catch {}
    if (typeof ctx?.cwd === "string") directory = ctx.cwd
    else if (directory === undefined) directory = process.cwd()
  }
  const begin = (ctx) => {
    if (!root && !interactive(ctx)) return false
    root = true
    remember(ctx)
    return true
  }

  return {
    on(event, data, ctx) {
      switch (event) {
        case "session_start":
        case "session_switch":
          if (!begin(ctx)) return
          cancelIdle()
          blocked = 0
          active = ctx?.isIdle?.() === false
          last = undefined
          report("start")
          if (active) publish()
          return
        case "agent_start":
          if (!begin(ctx)) return
          cancelIdle()
          active = true
          publish()
          return
        case "agent_settled":
          if (!root || ctx?.isIdle?.() === false) return
          cancelIdle()
          active = false
          blocked = 0
          publish()
          return
        case "agent_end":
          if (!root || !active || data?.willContinue === true) return
          cancelIdle()
          pendingIdle = setTimer(() => {
            pendingIdle = undefined
            active = false
            blocked = 0
            publish()
          }, settleMs)
          pendingIdle?.unref?.()
          return
        case "tool_approval_requested":
        case "ui_prompt_start":
          if (!begin(ctx)) return
          blocked += 1
          publish()
          return
        case "tool_approval_resolved":
        case "ui_prompt_end":
          if (!root) return
          blocked = Math.max(0, blocked - 1)
          publish()
          return
        case "tool_execution_start":
          if (data?.toolName !== "ask" || !begin(ctx)) return
          blocked += 1
          publish()
          return
        case "tool_execution_end":
          if (data?.toolName !== "ask" || !root) return
          blocked = Math.max(0, blocked - 1)
          publish()
          return
        case "session_shutdown":
          cancelIdle()
          if (root && (active || blocked > 0)) {
            active = false
            blocked = 0
            publish()
          }
          return
      }
    },
  }
}

const EVENTS = [
  "session_start",
  "session_switch",
  "agent_start",
  "agent_settled",
  "agent_end",
  "tool_approval_requested",
  "tool_approval_resolved",
  "ui_prompt_start",
  "ui_prompt_end",
  "tool_execution_start",
  "tool_execution_end",
  "session_shutdown",
]

export default function (pi) {
  // Set in Solo's terminals only.
  const hook = process.env.SOLO_AGENT_HOOK
  if (!hook) return
  const reporter = createReporter({ send: hookSender(hook, agentName()) })
  for (const event of EVENTS) {
    try {
      pi.on(event, (data, ctx) => reporter.on(event, data, ctx))
    } catch {
      // An event this version doesn't have.
    }
  }
}
