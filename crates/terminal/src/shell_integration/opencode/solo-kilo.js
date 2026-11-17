// SOLO: The Kilo Code CLI plugin Solo loads into `kilo` sessions started from
// its terminals, so Solo knows what Kilo is doing: working, waiting for the
// user, or done with the turn. Kilo's CLI is a fork of OpenCode with the same
// plugin events, so this is Solo's OpenCode 1 plugin reporting as "kilo".
//
// Solo puts it in `<its folder>/kilo-config/plugin/` and sets KILO_CONFIG_DIR
// to that folder in its terminals: Kilo adds that folder's plugins to the
// user's own (Kilo's config/paths.ts and config/plugin.ts); nothing is written
// to Kilo's config. Herdr's Kilo integration (Apache-2.0) installs a plugin
// into ~/.config/kilo/plugin instead.

import { createReporter, createResponseTimer, hookSender } from "../../opencode-plugin/solo-opencode.js"

export const SoloKiloStatus = async ({ directory }) => {
  // Set in Solo's terminals only.
  const hook = process.env.SOLO_AGENT_HOOK
  if (!hook) return {}
  const send = hookSender(hook, undefined, undefined, "kilo")
  const reporter = createReporter({ directory })
  const parents = new Map()
  const timer = createResponseTimer({ rootOf: (id) => (parents.has(id) ? parents.get(id) : id) })
  return {
    event: async ({ event }) => {
      const info = event?.properties?.info
      if (event?.type === "session.created" && typeof info?.id === "string" && typeof info.parentID === "string") {
        parents.set(info.id, info.parentID)
      }
      send(reporter.handle(event))
      send(timer.handle(event))
    },
  }
}
