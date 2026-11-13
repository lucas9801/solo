// SOLO: What Solo's OpenCode plugins share: turns OpenCode's events into
// Solo's agent reports and sends them with `solo-agent-hook report`
// (docs/agent-status-protocol.md), which writes them to the terminal OpenCode
// runs in. Used by tui.js (OpenCode 2) and solo-v1.js (OpenCode 1).
//
// Solo writes this file to its own temp folder; nothing goes into OpenCode's
// config. It has no imports of OpenCode's, so it can be tested with plain
// Node (solo-opencode.test.mjs).

import { spawn } from "node:child_process"

/**
 * What an event says the session is doing, or null:
 * - "start": a session was created;
 * - "turn": a turn started (OpenCode 2's execution events);
 * - "busy": the session is busy, maybe still in the same turn (OpenCode 1's
 *   session.status);
 * - "blocked": it waits for a permission or an answer;
 * - "idle": the turn ended (or failed, or was interrupted).
 */
export function stateOf(type, data) {
  switch (type) {
    case "session.created":
      return "start"
    case "session.execution.started":
      return "turn"
    case "session.status": {
      const status = typeof data?.status === "string" ? data.status : data?.status?.type
      if (status === "busy" || status === "retry") return "busy"
      if (status === "idle") return "idle"
      return null
    }
    case "session.execution.succeeded":
    case "session.execution.failed":
    case "session.execution.interrupted":
    case "session.idle":
    case "session.error":
      return "idle"
    // permission.updated is OpenCode 1's older name; question.asked is its
    // question tool; OpenCode 2 asks questions with forms.
    case "permission.asked":
    case "permission.updated":
    case "question.asked":
    case "form.created":
      return "blocked"
    default:
      return null
  }
}

/** The session an event is about. */
export function sessionOf(type, data) {
  if (!data || typeof data !== "object") return undefined
  if (typeof data.sessionID === "string") return data.sessionID
  if (typeof data.info?.id === "string") return data.info.id
  if (type.startsWith("session.") && typeof data.id === "string") return data.id
  return undefined
}

/** A session id as Solo's hook takes it. */
export function isSessionId(id) {
  return typeof id === "string" && /^[A-Za-z0-9_-]{1,128}$/.test(id)
}

/**
 * Keeps what's been reported for each session and decides what to report
 * for each event.
 *
 * - `rootOf(id)`: the top session of a subagent's session (default: from the
 *   parents seen in session.created events);
 * - `accept(session)`: whether this terminal shows that session (OpenCode 2's
 *   shared server sends every client's events);
 * - `directory`: where OpenCode works, when an event doesn't say.
 */
export function createReporter({ rootOf, accept = () => true, directory } = {}) {
  const parents = new Map()
  const directories = new Map()
  const states = new Map()
  const requests = new Map()
  const root =
    rootOf ??
    ((id) => {
      let session = id
      for (let depth = 0; depth < 32 && parents.has(session); depth++) session = parents.get(session)
      return session
    })

  const report = (state, session, eventDirectory) => {
    states.set(session, state)
    return {
      state,
      session,
      directory: eventDirectory ?? directories.get(session) ?? directory,
    }
  }

  return {
    /** The report for an OpenCode event (`{type, data}` in OpenCode 2, `{type, properties}` in 1), or null. */
    handle(event) {
      if (!event || typeof event.type !== "string") return null
      const type = event.type
      const data = event.data ?? event.properties ?? {}
      if (type === "session.created" || type === "session.updated") {
        const info = data.info ?? data
        if (typeof info?.id === "string") {
          if (typeof info.parentID === "string") parents.set(info.id, info.parentID)
          const where = info.directory ?? info.location?.directory
          if (typeof where === "string") directories.set(info.id, where)
        }
      }
      const state = stateOf(type, data)
      const id = state && sessionOf(type, data)
      if (!id) return null
      const session = root(id) ?? id
      // A subagent's session: only its questions are the user's to answer.
      if (session !== id && state !== "blocked") return null
      if (!accept(session)) return null
      const last = states.get(session)
      const inTurn = last === "working" || last === "blocked"
      const where = typeof event.location?.directory === "string" ? event.location.directory : undefined
      switch (state) {
        case "start":
          return last === undefined ? report("start", session, where) : null
        case "turn":
          return last === "working" ? null : report("working", session, where)
        case "busy":
          return inTurn ? null : report("working", session, where)
        case "idle":
          return inTurn ? report("idle", session, where) : null
        case "blocked": {
          const request = typeof data.id === "string" ? data.id : undefined
          if (last === "blocked" && request !== undefined && requests.get(session) === request) return null
          requests.set(session, request)
          return report("blocked", session, where)
        }
      }
      return null
    },

    /**
     * The terminal shows `session` now (OpenCode 2's TUI switched to it):
     * reports it as started, and as working if it is.
     */
    focus(session, busy, where) {
      const reports = [report("start", session, where)]
      if (busy) reports.push(report("working", session, where))
      return reports
    },
  }
}

/** Whether a session status (OpenCode 2's `data.session.status`) is a busy one. */
export function isBusy(status) {
  const type = typeof status === "string" ? status : status?.type
  return type === "busy" || type === "retry"
}

/** The arguments of `solo-agent-hook` for a report. */
export function reportArguments(report, seq) {
  const args = ["report", "--state", report.state, "--agent", "opencode", "--seq", String(seq)]
  if (isSessionId(report.session)) args.push("--session", report.session)
  if (typeof report.directory === "string" && report.directory.trim() !== "") {
    args.push("--directory", report.directory)
  }
  return args
}

/**
 * Sends reports by starting `hook` (solo-agent-hook) for each, in the
 * background. Each gets a `--seq` above the last one's, as the hooks may
 * finish out of order.
 */
export function hookSender(hook, spawnProcess = spawn, now = Date.now) {
  let last = 0
  return (report) => {
    if (!hook || !report) return
    const seq = Math.max(now(), last + 1)
    last = seq
    try {
      const child = spawnProcess(hook, reportArguments(report, seq), {
        stdio: "ignore",
        windowsHide: true,
      })
      child?.on?.("error", () => {})
      child?.unref?.()
    } catch {
      // Not in a Solo terminal any more, or the hook is gone: nothing to tell.
    }
  }
}
