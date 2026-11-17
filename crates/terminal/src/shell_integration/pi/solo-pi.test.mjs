// SOLO: Tests of Solo's Pi / oh-my-pi extension with fake events, without Pi:
// `node --test crates/terminal/src/shell_integration/pi/solo-pi.test.mjs`.

import assert from "node:assert/strict"
import { test } from "node:test"

import register, { agentName, createReporter, hookSender, interactive, reportArguments } from "./solo-pi.ts"

const tui = (id = "s1", idle = true) => ({
  mode: "tui",
  cwd: "/repo",
  isIdle: () => idle,
  sessionManager: { getSessionId: () => id },
})

function reporter() {
  const sent = []
  const timers = []
  const r = createReporter({
    send: (report) => sent.push(`${report.state} ${report.session}`),
    setTimer: (fn) => (timers.push(fn), timers.length),
    clearTimer: (handle) => (timers[handle - 1] = null),
  })
  const fire = () => timers.splice(0).forEach((fn) => fn?.())
  return { r, sent, fire }
}

test("a Pi turn: start, working, idle when settled", () => {
  const { r, sent } = reporter()
  r.on("session_start", { reason: "startup" }, tui())
  r.on("agent_start", {}, tui("s1", false))
  r.on("agent_end", {}, tui("s1", false))
  r.on("agent_settled", {}, tui())
  assert.deepEqual(sent, ["start s1", "working s1", "idle s1"])
})

test("an OMP turn ends after agent_end once nothing follows", () => {
  const { r, sent, fire } = reporter()
  const ctx = { hasUI: true, cwd: "/repo", sessionManager: { getSessionId: () => "o1" } }
  r.on("session_start", {}, ctx)
  r.on("agent_start", {}, ctx)
  r.on("agent_end", { willContinue: true }, ctx)
  r.on("agent_end", {}, ctx)
  assert.deepEqual(sent, ["start o1", "working o1"])
  fire()
  assert.deepEqual(sent, ["start o1", "working o1", "idle o1"])
})

test("a new run within the settle time keeps working", () => {
  const { r, sent, fire } = reporter()
  r.on("session_start", {}, tui())
  r.on("agent_start", {}, tui())
  r.on("agent_end", {}, tui())
  r.on("agent_start", {}, tui())
  fire()
  assert.deepEqual(sent, ["start s1", "working s1"])
})

test("approvals, prompts and the ask tool wait for the user", () => {
  const { r, sent } = reporter()
  r.on("session_start", {}, tui())
  r.on("agent_start", {}, tui())
  r.on("tool_approval_requested", { toolName: "bash" }, tui())
  r.on("tool_approval_resolved", {}, tui())
  r.on("tool_execution_start", { toolName: "read" }, tui())
  r.on("tool_execution_start", { toolName: "ask" }, tui())
  r.on("tool_execution_end", { toolName: "ask" }, tui())
  r.on("ui_prompt_start", {}, tui())
  r.on("ui_prompt_end", {}, tui())
  assert.deepEqual(sent, [
    "start s1",
    "working s1",
    "blocked s1",
    "working s1",
    "blocked s1",
    "working s1",
    "blocked s1",
    "working s1",
  ])
})

test("headless sessions report nothing", () => {
  const { r, sent } = reporter()
  r.on("session_start", {}, { mode: "rpc", hasUI: true })
  r.on("agent_start", {}, { mode: "rpc" })
  r.on("agent_end", {}, { mode: "rpc" })
  assert.deepEqual(sent, [])
  assert.equal(interactive({ mode: "print" }), false)
  assert.equal(interactive({ hasUI: true }), true)
})

test("reports run the hook with increasing order", () => {
  const calls = []
  const send = hookSender("/x/solo-agent-hook", "omp", (program, args) => {
    calls.push([program, ...args])
    return { on() {}, unref() {} }
  }, () => 5)
  send({ state: "working", session: "o1", directory: "/repo" })
  send({ state: "idle", session: "bad id;" })
  assert.deepEqual(calls, [
    ["/x/solo-agent-hook", "report", "--state", "working", "--agent", "omp", "--seq", "5", "--session", "o1", "--directory", "/repo"],
    ["/x/solo-agent-hook", "report", "--state", "idle", "--agent", "omp", "--seq", "6"],
  ])
  assert.deepEqual(reportArguments("pi", { state: "start" }, 1), ["report", "--state", "start", "--agent", "pi", "--seq", "1"])
})

test("tells Pi from oh-my-pi", () => {
  assert.equal(agentName(["node", "/usr/lib/node_modules/@mariozechner/pi-coding-agent/dist/cli.js"]), "pi")
  assert.equal(agentName(["bun", "C:\\Users\\me\\.bun\\bin\\omp.exe"]), "omp")
  assert.equal(agentName(["bun", "/opt/oh-my-pi/cli.ts"]), "omp")
})

test("does nothing outside Solo", () => {
  const saved = process.env.SOLO_AGENT_HOOK
  delete process.env.SOLO_AGENT_HOOK
  const events = []
  register({ on: (event) => events.push(event) })
  if (saved !== undefined) process.env.SOLO_AGENT_HOOK = saved
  assert.deepEqual(events, [])
})
