// SOLO: Tests of Solo's OpenCode plugins with fake events, without OpenCode:
// `node --test crates/terminal/src/shell_integration/opencode/`.

import assert from "node:assert/strict"
import { mkdtempSync, mkdirSync, copyFileSync, writeFileSync, readFileSync, existsSync, rmSync } from "node:fs"
import { tmpdir } from "node:os"
import { join, dirname } from "node:path"
import { fileURLToPath, pathToFileURL } from "node:url"
import { test } from "node:test"

import { createReporter, hookSender, isBusy, reportArguments, sessionOf, stateOf } from "./solo-opencode.js"

const here = dirname(fileURLToPath(import.meta.url))
const states = (reports) => reports.filter(Boolean).map((report) => `${report.state} ${report.session}`)

test("maps events to states", () => {
  assert.equal(stateOf("session.execution.started", {}), "turn")
  assert.equal(stateOf("session.status", { sessionID: "s", status: { type: "busy" } }), "busy")
  assert.equal(stateOf("session.status", { sessionID: "s", status: { type: "retry", attempt: 2 } }), "busy")
  assert.equal(stateOf("session.status", { sessionID: "s", status: { type: "idle" } }), "idle")
  assert.equal(stateOf("session.execution.interrupted", {}), "idle")
  assert.equal(stateOf("session.error", {}), "idle")
  assert.equal(stateOf("permission.asked", {}), "blocked")
  assert.equal(stateOf("permission.updated", {}), "blocked")
  assert.equal(stateOf("question.asked", {}), "blocked")
  assert.equal(stateOf("form.created", {}), "blocked")
  assert.equal(stateOf("permission.replied", {}), null)
  assert.equal(stateOf("message.part.updated", {}), null)
  assert.equal(sessionOf("permission.asked", { id: "per_1", sessionID: "ses_a" }), "ses_a")
  assert.equal(sessionOf("session.created", { info: { id: "ses_a" } }), "ses_a")
  assert.equal(sessionOf("session.created", { id: "ses_a" }), "ses_a")
  assert.equal(sessionOf("permission.asked", { id: "per_1" }), undefined)
  assert.ok(isBusy({ type: "busy" }) && isBusy("retry") && !isBusy({ type: "idle" }) && !isBusy(undefined))
})

test("an OpenCode 1 turn with a permission and a subagent", () => {
  const reporter = createReporter({ directory: "/work" })
  const event = (type, properties) => reporter.handle({ type, properties })
  const reports = [
    event("session.created", { info: { id: "ses_a", directory: "/work/app" } }),
    event("session.status", { sessionID: "ses_a", status: { type: "busy" } }),
    // Busy again within the turn, after a retry.
    event("session.status", { sessionID: "ses_a", status: { type: "retry" } }),
    event("session.status", { sessionID: "ses_a", status: { type: "busy" } }),
    event("permission.asked", { id: "per_1", sessionID: "ses_a" }),
    // The same request again, then the next one.
    event("permission.asked", { id: "per_1", sessionID: "ses_a" }),
    event("permission.replied", { sessionID: "ses_a", requestID: "per_1" }),
    // A subagent's session: its own status and end don't count; its questions do.
    event("session.created", { info: { id: "ses_child", parentID: "ses_a" } }),
    event("session.status", { sessionID: "ses_child", status: { type: "busy" } }),
    event("question.asked", { id: "que_1", sessionID: "ses_child" }),
    event("session.idle", { sessionID: "ses_child" }),
    event("session.status", { sessionID: "ses_a", status: { type: "idle" } }),
    event("session.idle", { sessionID: "ses_a" }),
    event("session.status", { sessionID: "ses_a", status: { type: "busy" } }),
    event("session.error", { sessionID: "ses_a", error: { name: "APIError" } }),
  ]
  assert.deepEqual(states(reports), [
    "start ses_a",
    "working ses_a",
    "blocked ses_a",
    "blocked ses_a",
    "idle ses_a",
    "working ses_a",
    "idle ses_a",
  ])
  assert.equal(reports[0].directory, "/work/app")
})

test("OpenCode 2 events, for the session this TUI shows only", () => {
  let shown = "ses_a"
  const reporter = createReporter({
    rootOf: (id) => (id === "ses_child" ? "ses_a" : id),
    accept: (session) => session === shown,
    directory: "/work",
  })
  const event = (type, data, location) => reporter.handle({ type, data, location })
  const reports = [
    ...reporter.focus("ses_a", false),
    event("session.execution.started", { sessionID: "ses_a" }, { directory: "/work/app" }),
    // Another client's session on the shared server.
    event("session.execution.started", { sessionID: "ses_other" }),
    event("permission.asked", { id: "per_1", sessionID: "ses_child" }),
    event("session.execution.succeeded", { sessionID: "ses_child" }),
    event("session.execution.succeeded", { sessionID: "ses_a" }),
    event("session.idle", { sessionID: "ses_a" }),
    event("session.execution.started", { sessionID: "ses_a" }),
    event("form.created", { id: "frm_1", sessionID: "ses_a", title: "Pick one" }),
    event("session.execution.interrupted", { sessionID: "ses_a" }),
  ]
  assert.deepEqual(states(reports), [
    "start ses_a",
    "working ses_a",
    "blocked ses_a",
    "idle ses_a",
    "working ses_a",
    "blocked ses_a",
    "idle ses_a",
  ])
  assert.equal(reports[1].directory, "/work/app")
  // Switching to a busy session.
  shown = "ses_b"
  assert.deepEqual(states(reporter.focus("ses_b", true)), ["start ses_b", "working ses_b"])
  assert.equal(reporter.handle({ type: "session.execution.started", data: { sessionID: "ses_b" } }), null)
  assert.equal(reporter.handle({ type: "session.execution.failed", data: { sessionID: "ses_b" } }).state, "idle")
})

test("hook arguments", () => {
  assert.deepEqual(reportArguments({ state: "working", session: "ses_4f2a1c", directory: "C:\\work dir" }, 7), [
    "report", "--state", "working", "--agent", "opencode", "--seq", "7",
    "--session", "ses_4f2a1c", "--directory", "C:\\work dir",
  ])
  // Nothing that could break the escape sequence.
  assert.deepEqual(reportArguments({ state: "idle", session: "a;b\u0007", directory: " " }, 1), [
    "report", "--state", "idle", "--agent", "opencode", "--seq", "1",
  ])
  const calls = []
  const send = hookSender("hook.exe", (program, args) => calls.push([program, args]), () => 100)
  send({ state: "working", session: "ses_a" })
  send({ state: "idle", session: "ses_a" })
  send(null)
  assert.deepEqual(calls.map(([program, args]) => [program, args[2], args[6]]), [
    ["hook.exe", "working", "100"],
    ["hook.exe", "idle", "101"],
  ])
  // A hook that can't start doesn't take OpenCode down.
  hookSender("hook.exe", () => {
    throw new Error("ENOENT")
  })({ state: "idle" })
})

/** Waits until `path` has `lines` lines. */
async function linesOf(path, lines) {
  for (let attempt = 0; attempt < 100; attempt++) {
    if (existsSync(path)) {
      const text = readFileSync(path, "utf8").trim().split("\n").filter(Boolean)
      if (text.length >= lines) return text
    }
    await new Promise((resolve) => setTimeout(resolve, 50))
  }
  return existsSync(path) ? readFileSync(path, "utf8").trim().split("\n") : []
}

/**
 * The plugin files laid out as Solo writes them, with a stand-in for
 * solo-agent-hook: Node running `report`, a script that logs its arguments.
 */
function layout() {
  const root = mkdtempSync(join(tmpdir(), "solo-opencode-test-"))
  mkdirSync(join(root, "opencode-plugin"))
  mkdirSync(join(root, "opencode-config", "plugins"), { recursive: true })
  copyFileSync(join(here, "solo-opencode.js"), join(root, "opencode-plugin", "solo-opencode.js"))
  copyFileSync(join(here, "tui.js"), join(root, "opencode-plugin", "tui.js"))
  copyFileSync(join(here, "solo-v1.js"), join(root, "opencode-config", "plugins", "solo.js"))
  // What OpenCode 2 resolves `@opencode/plugin/tui` to, reduced.
  const sdk = join(root, "node_modules", "@opencode", "plugin")
  mkdirSync(sdk, { recursive: true })
  writeFileSync(join(sdk, "package.json"), JSON.stringify({ name: "@opencode/plugin", type: "module", exports: { "./tui": "./tui.js" } }))
  writeFileSync(join(sdk, "tui.js"), "export const Plugin = { define: (plugin) => plugin }\n")
  const log = join(root, "reports.log")
  writeFileSync(
    join(root, "report"),
    `require("fs").appendFileSync(${JSON.stringify(log)}, JSON.stringify(["report", ...process.argv.slice(2)]) + "\\n")\n`,
  )
  return { root, log }
}

test("the OpenCode 1 plugin reports through the hook", async () => {
  const { root, log } = layout()
  const cwd = process.cwd()
  const hook = process.env.SOLO_AGENT_HOOK
  try {
    process.chdir(root)
    process.env.SOLO_AGENT_HOOK = process.execPath
    const module = await import(pathToFileURL(join(root, "opencode-config", "plugins", "solo.js")))
    assert.deepEqual(Object.keys(module), ["SoloStatus"])
    const hooks = await module.SoloStatus({ directory: root })
    await hooks.event({ event: { type: "session.status", properties: { sessionID: "ses_a", status: { type: "busy" } } } })
    await hooks.event({ event: { type: "permission.asked", properties: { id: "per_1", sessionID: "ses_a" } } })
    await hooks.event({ event: { type: "session.idle", properties: { sessionID: "ses_a" } } })
    const lines = (await linesOf(log, 3)).map((line) => JSON.parse(line))
    assert.deepEqual(lines.map((args) => args[2]).sort(), ["blocked", "idle", "working"])
    for (const args of lines) {
      assert.deepEqual(args.slice(3, 5), ["--agent", "opencode"])
      assert.deepEqual(args.slice(7), ["--session", "ses_a", "--directory", root])
    }
    // Outside Solo it does nothing.
    delete process.env.SOLO_AGENT_HOOK
    assert.deepEqual(await module.SoloStatus({ directory: root }), {})
  } finally {
    process.chdir(cwd)
    if (hook === undefined) delete process.env.SOLO_AGENT_HOOK
    else process.env.SOLO_AGENT_HOOK = hook
    rmSync(root, { recursive: true, force: true })
  }
})

test("the OpenCode 2 TUI plugin follows the session it shows", async () => {
  const { root, log } = layout()
  const cwd = process.cwd()
  const hook = process.env.SOLO_AGENT_HOOK
  try {
    process.chdir(root)
    process.env.SOLO_AGENT_HOOK = process.execPath
    const plugin = (await import(pathToFileURL(join(root, "opencode-plugin", "tui.js")))).default
    assert.equal(plugin.id, "solo.status")
    let listener
    let route = { type: "home" }
    const context = {
      location: { directory: root },
      ui: { router: { current: () => route } },
      data: {
        listen: (callback) => {
          listener = callback
          return () => (listener = undefined)
        },
        session: { root: (id) => id, status: (id) => (id === "ses_busy" ? { type: "busy" } : { type: "idle" }) },
      },
    }
    const cleanup = plugin.setup(context)
    // Not shown: nothing.
    listener({ details: { type: "session.execution.started", data: { sessionID: "ses_a" } } })
    route = { type: "session", sessionID: "ses_busy" }
    await new Promise((resolve) => setTimeout(resolve, 400))
    listener({ details: { type: "session.execution.succeeded", data: { sessionID: "ses_busy" } } })
    const lines = (await linesOf(log, 3)).map((line) => JSON.parse(line))
    assert.deepEqual(lines.map((args) => `${args[2]} ${args[8]}`).sort(), [
      "idle ses_busy",
      "start ses_busy",
      "working ses_busy",
    ])
    cleanup()
    assert.equal(listener, undefined)
  } finally {
    process.chdir(cwd)
    if (hook === undefined) delete process.env.SOLO_AGENT_HOOK
    else process.env.SOLO_AGENT_HOOK = hook
    rmSync(root, { recursive: true, force: true })
  }
})
