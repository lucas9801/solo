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

/** Message parts that are the model's output: its first one is its first token. */
const OUTPUT_PARTS = new Set(["text", "reasoning", "tool"])
/** How many messages are timed at once, at most. */
const MAX_TIMED_MESSAGES = 200

/**
 * Times the model's answers from OpenCode's message events, by when this
 * process sees them: an assistant message is the request, its first output
 * part (or delta) the first token, and its completion the last. Solo shows
 * the time to the first token and the output speed.
 *
 * Unverified against a real OpenCode: the events' shapes are from its
 * message schema (`time.created`, `time.completed`, `tokens.output`,
 * `tokens.reasoning`, `variant`), not from a recorded session.
 *
 * - `now`: the clock, in ms;
 * - `rootOf(id)`, `accept(session)`: as for `createReporter`. Only the
 *   main conversation's messages are timed, not subagents'.
 */
export function createResponseTimer({ now = Date.now, rootOf = (id) => id, accept = () => true } = {}) {
  const messages = new Map()
  const variants = new Map()
  const number = (value) => (typeof value === "number" && Number.isFinite(value) && value >= 0 ? value : undefined)
  const main = (session) => typeof session === "string" && (rootOf(session) ?? session) === session && accept(session)

  const track = (id, session, start) => {
    if (messages.size >= MAX_TIMED_MESSAGES) messages.delete(messages.keys().next().value)
    const message = { session, start, first: undefined }
    messages.set(id, message)
    return message
  }

  const output = (messageID, at) => {
    const message = messages.get(messageID)
    if (message && message.first === undefined) message.first = at
  }

  return {
    /** The response report for an OpenCode event, or null. */
    handle(event) {
      if (!event || typeof event.type !== "string") return null
      const data = event.data ?? event.properties ?? {}
      const at = now()
      switch (event.type) {
        case "message.part.updated": {
          const part = data.part ?? data
          if (!OUTPUT_PARTS.has(part?.type) || typeof part.messageID !== "string") return null
          // A text part is created before its first text arrives.
          const empty = part.type !== "tool" && !part.text && !data.delta
          if (!empty) output(part.messageID, at)
          return null
        }
        case "message.part.delta":
          if (typeof data.messageID === "string" && data.delta) output(data.messageID, at)
          return null
        case "message.updated": {
          const info = data.info ?? data
          if (typeof info?.id !== "string" || !main(info.sessionID)) return null
          if (typeof info.variant === "string") variants.set(info.sessionID, info.variant)
          if (info.role !== "assistant") return null
          let message = messages.get(info.id)
          if (!message) {
            if (info.time?.completed !== undefined) return null
            // OpenCode's own time of the request, when it's this machine's.
            const created = number(info.time?.created)
            message = track(info.id, info.sessionID, created !== undefined && created <= at ? created : at)
          }
          if (number(info.time?.completed) === undefined || message.done) return null
          message.done = true
          messages.delete(info.id)
          if (info.error) return null
          const measurements = {}
          if (message.first !== undefined) {
            measurements.firstTokenMs = Math.round(message.first - message.start)
            measurements.generationMs = Math.round(at - message.first)
          }
          const tokens = number(info.tokens?.output)
          if (tokens !== undefined) measurements.outputTokens = tokens + (number(info.tokens?.reasoning) ?? 0)
          const variant = variants.get(info.sessionID)
          if (typeof variant === "string") measurements.effort = variant
          if (Object.keys(measurements).length === 0) return null
          return { state: "response", session: info.sessionID, ...measurements }
        }
        default:
          return null
      }
    },
  }
}

/** Whether a session status (OpenCode 2's `data.session.status`) is a busy one. */
export function isBusy(status) {
  const type = typeof status === "string" ? status : status?.type
  return type === "busy" || type === "retry"
}

/** The arguments of `solo-agent-hook` for a report. */
export function reportArguments(report, seq, agent = "opencode") {
  const args = ["report", "--state", report.state, "--agent", agent, "--seq", String(seq)]
  if (isSessionId(report.session)) args.push("--session", report.session)
  if (typeof report.directory === "string" && report.directory.trim() !== "") {
    args.push("--directory", report.directory)
  }
  if (report.state === "response") {
    const count = (value) => Number.isInteger(value) && value >= 0
    if (count(report.firstTokenMs)) args.push("--first-token-ms", String(report.firstTokenMs))
    if (count(report.outputTokens)) args.push("--output-tokens", String(report.outputTokens))
    if (count(report.generationMs)) args.push("--generation-ms", String(report.generationMs))
    if (typeof report.effort === "string" && /^[A-Za-z0-9_-]{1,24}$/.test(report.effort)) {
      args.push("--effort", report.effort)
    }
  }
  return args
}

/**
 * Sends reports by starting `hook` (solo-agent-hook) for each, in the
 * background. Each gets a `--seq` above the last one's, as the hooks may
 * finish out of order.
 */
export function hookSender(hook, spawnProcess = spawn, now = Date.now, agent = "opencode") {
  let last = 0
  return (report) => {
    if (!hook || !report) return
    const seq = Math.max(now(), last + 1)
    last = seq
    try {
      const child = spawnProcess(hook, reportArguments(report, seq, agent), {
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
