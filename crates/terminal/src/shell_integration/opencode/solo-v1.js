// SOLO: The OpenCode 1 plugin Solo loads into `opencode` sessions started from
// its terminals (from the folder in OPENCODE_CONFIG_DIR, for that session
// only), so Solo knows what OpenCode is doing: working, waiting for the user,
// or done with the turn. OpenCode 1 runs its server in the TUI's process, so
// every event is this terminal's. See https://docs.opencode.ai/docs/plugins/.

import { createEditReporter, createReporter, createResponseTimer, hookSender } from "../../opencode-plugin/solo-opencode.js"

export const SoloStatus = async ({ directory }) => {
  // Set in Solo's terminals only.
  const hook = process.env.SOLO_AGENT_HOOK
  if (!hook) return {}
  const send = hookSender(hook)
  const reporter = createReporter({ directory })
  // Subagents' sessions have a parent; the timer leaves them out.
  const parents = new Map()
  const rootOf = (id) => (parents.has(id) ? parents.get(id) : id)
  const timer = createResponseTimer({ rootOf })
  const edits = createEditReporter({ rootOf, directory })
  return {
    event: async ({ event }) => {
      const info = event?.properties?.info
      if (event?.type === "session.created" && typeof info?.id === "string" && typeof info.parentID === "string") {
        parents.set(info.id, info.parentID)
      }
      send(reporter.handle(event))
      send(timer.handle(event))
      for (const report of edits.handle(event)) send(report)
    },
  }
}
