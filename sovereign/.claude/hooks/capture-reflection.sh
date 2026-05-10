#!/bin/sh
# sovereign capture-reflection — Stop hook for Claude Code.
#
# Auto-records a session-end reflection note describing what changed
# during the session (uncommitted diff + recent commits + branch).
# Runs non-interactively — no TTY needed — so it actually fires
# inside Claude Code's Stop event (the previous interactive version
# was silently dead because Stop runs in a non-TTY context).
#
# The Rust binary does the work; the hook is just a tee-up:
#   - Resolves repo root from cwd.
#   - Writes via NoteStore::write_reflection_scoped at
#     ~/.sovereign/notes.db.
#   - Bails silently if nothing changed (no diff, no recent commits).
#
# Opt-out: SOVEREIGN_NO_REFLECTION=1.

[ "${SOVEREIGN_NO_REFLECTION:-0}" = "1" ] && exit 0

SOVEREIGN_BIN="${SOVEREIGN_BIN:-$HOME/.local/bin/sovereign}"
[ -x "$SOVEREIGN_BIN" ] || exit 0

# Stop hook receives a JSON payload on stdin (session_id,
# transcript_path, hook_event_name). We don't need any of it for v0
# — the reflection captures git state, not transcript content. Drain
# stdin to avoid SIGPIPE on the parent.
cat >/dev/null 2>&1

# Forward the active feature id when set so the reflection is scoped
# to the same feature its sibling notes will be.
FEATURE_ARG=""
if [ -n "${SOVEREIGN_FEATURE_ID:-}" ]; then
    FEATURE_ARG="--feature-id $SOVEREIGN_FEATURE_ID"
fi

"$SOVEREIGN_BIN" code reflect --quiet --hours 4 $FEATURE_ARG 2>/dev/null

exit 0
