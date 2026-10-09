#!/bin/sh
# euphemia-pre-edit.sh — PreToolUse (Edit|Write) advisory: other sessions'
# active claims on the file about to change, from euphemia's commons
# (campaign euphemia, Stage A; eu-e5-surfaces).
#
# Guarded: a host without euphemia, or a checkout in no commons, prints
# nothing. NEVER BLOCKS: euphemia's hooks always exit 0, and so does this.
# It never repeats an item within a session (euphemia keeps that state).
#
# Harness-neutral, like every script here: the JSON envelope arrives on
# stdin (Claude Code) or in $SOVEREIGN_HOOK_INPUT (the opencode adapter sets
# both), and `euphemia hook pre-edit` reads either. pi's adapter
# (.pi/extensions/sovereign-hooks/index.ts) has no pre-edit event, so pi
# sessions get the session-start brief and not this: a gap stated, not
# bridged.
command -v euphemia >/dev/null 2>&1 || exit 0
euphemia hook pre-edit 2>/dev/null
exit 0
