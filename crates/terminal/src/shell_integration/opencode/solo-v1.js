// SOLO: The OpenCode 1 plugin Solo loads into `opencode` sessions started from
// its terminals (from the folder in OPENCODE_CONFIG_DIR, for that session
// only), so Solo knows what OpenCode is doing: working, waiting for the user,
// or done with the turn. OpenCode 1 runs its server in the TUI's process, so
// every event is this terminal's. See https://docs.opencode.ai/docs/plugins/.

import { createReporter, hookSender } from "../../opencode-plugin/solo-opencode.js"

export const SoloStatus = async ({ directory }) => {
  // Set in Solo's terminals only.
  const hook = process.env.SOLO_AGENT_HOOK
  if (!hook) return {}
  const send = hookSender(hook)
  const reporter = createReporter({ directory })
  return {
    event: async ({ event }) => {
      send(reporter.handle(event))
    },
  }
}
