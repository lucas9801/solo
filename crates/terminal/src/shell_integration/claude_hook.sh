#!/bin/sh
# Solo: a Claude Code hook that tells the terminal what Claude is doing, with
# OSC 6973 ; agent ; nonce=<the terminal's nonce> ; <kind> ; <ms since the epoch>.
# The terminal ignores reports without its agent key (SOLO_AGENT_TOKEN). Written to
# the terminal of the shell Claude was started from (SOLO_AGENT_TTY), or else the
# controlling one.

exec 2>/dev/null
cat >/dev/null
# Hooks run in the background and may finish out of order; the terminal orders
# them by this. Not every date has %N.
sent_at=$(date +%s%3N)
case $sent_at in
    '' | *[!0-9]*) sent_at= ;;
esac
nonce=${SOLO_AGENT_TOKEN:-}
case $nonce in
    *[!0-9A-Za-z]*) nonce= ;;
esac
printf '\033]6973;agent;nonce=%s;%s;%s\007' "$nonce" "$1" "$sent_at" >"${SOLO_AGENT_TTY:-/dev/tty}"
exit 0
