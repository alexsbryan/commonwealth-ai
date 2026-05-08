#!/bin/sh
# sovereign inject-brief — UserPromptSubmit hook for Claude Code.
#
# Replaces inject-notes.sh. Renders a working-set brief that includes
# active notes inline (one canonical injection, not two). Runs the
# `sovereign code brief` CLI directly — no daemon HTTP round-trip
# required, so it works offline.
#
# Fails silently when the CLI is missing or git fails so an offline
# session is unaffected.
#
# Opt-out: SOVEREIGN_NO_BRIEF=1

[ "${SOVEREIGN_NO_BRIEF:-0}" = "1" ] && exit 0

SOVEREIGN_BIN="${SOVEREIGN_BIN:-$HOME/.local/bin/sovereign}"
[ -x "$SOVEREIGN_BIN" ] || exit 0

# ATOS scope-aware: forward the active feature_id when set so the
# notes section pulls feature-scoped notes alongside globals.
FEATURE_ARG=""
if [ -n "${SOVEREIGN_FEATURE_ID:-}" ]; then
    FEATURE_ARG="--feature-id $SOVEREIGN_FEATURE_ID"
fi

# Atlas id default: derive from the cwd's basename. The brief
# assembler tolerates a missing atlas (skips the structural section)
# so this is best-effort.
ATLAS_ID="${SOVEREIGN_ATLAS_ID:-$(basename "$PWD")}"

# Run with a 1.5s timeout when available (gtimeout on macOS via
# coreutils, timeout on Linux). Without one we just exec — the brief
# is fast (<100ms typical) and the hook fails silently via `|| exit 0`
# if anything goes wrong.
if command -v gtimeout >/dev/null 2>&1; then
    TIMEOUT="gtimeout 1.5"
elif command -v timeout >/dev/null 2>&1; then
    TIMEOUT="timeout 1.5"
else
    TIMEOUT=""
fi

$TIMEOUT "$SOVEREIGN_BIN" code brief \
    --strategy branch \
    --atlas-id "$ATLAS_ID" \
    --budget 1500 \
    $FEATURE_ARG 2>/dev/null || exit 0
