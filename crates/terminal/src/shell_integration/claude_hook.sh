#!/bin/sh
# Solo: a Claude Code hook that tells the terminal what Claude is doing, with
# OSC 6973 ; agent ; <kind> ; <ms since the epoch>. Written to the terminal of
# the shell Claude was started from (SOLO_AGENT_TTY), or else the controlling one.

exec 2>/dev/null
cat >/dev/null
# Hooks run in the background and may finish out of order; the terminal orders
# them by this. Not every date has %N.
sent_at=$(date +%s%3N)
case $sent_at in
    '' | *[!0-9]*) sent_at= ;;
esac
printf '\033]6973;agent;%s;%s\007' "$1" "$sent_at" >"${SOLO_AGENT_TTY:-/dev/tty}"
exit 0
