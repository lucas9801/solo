// SOLO: The OpenCode 2 TUI plugin Solo loads into `opencode` sessions started
// from its terminals (through OPENCODE_CLI_CONFIG_CONTENT, for that session
// only), so Solo knows what OpenCode is doing: working, waiting for the user,
// or done with the turn.
//
// It's a TUI plugin because the TUI is what runs in the terminal; OpenCode 2's
// server is shared by every client, so its events are filtered down to the
// session this TUI shows. See https://opencode.ai/docs/build/plugins/cli/.

import { Plugin } from "@opencode/plugin/tui"
import { createReporter, hookSender, isBusy } from "./solo-opencode.js"

/** How often to check which session the TUI shows. */
const FOCUS_POLL_MS = 300

export default Plugin.define({
  id: "solo.status",
  setup(context) {
    // Set in Solo's terminals only.
    const hook = process.env.SOLO_AGENT_HOOK
    if (!hook) return
    const send = hookSender(hook)
    let shown
    const directory = () => context.location?.directory
    const reporter = createReporter({
      rootOf: (id) => {
        try {
          return context.data.session.root(id) ?? id
        } catch {
          return id
        }
      },
      accept: (session) => session === shown,
      directory: directory(),
    })

    const stopListening = context.data.listen((event) => {
      send(reporter.handle(event?.details ?? event))
    })

    const checkFocus = () => {
      let route
      try {
        route = context.ui.router.current()
      } catch {
        return
      }
      const session = route?.type === "session" ? route.sessionID : undefined
      if (session === shown) return
      shown = session
      if (!session) return
      let status
      try {
        status = context.data.session.status(session)
      } catch {}
      for (const report of reporter.focus(session, isBusy(status), directory())) send(report)
    }
    checkFocus()
    const timer = setInterval(checkFocus, FOCUS_POLL_MS)
    timer.unref?.()

    return () => {
      clearInterval(timer)
      stopListening?.()
    }
  },
})
