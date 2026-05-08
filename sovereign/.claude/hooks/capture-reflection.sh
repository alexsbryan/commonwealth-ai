#!/bin/sh
# sovereign capture-reflection — Stop hook for Claude Code.
#
# Triggered when a session ends. Prompts the engineer for a one-line
# decision/invariant/todo. If they enter content, the hook writes it
# to NoteStore via the daemon's MCP `note` tool so it surfaces in
# the next session's brief.
#
# Skip: hit Enter at the prompt, or set SOVEREIGN_NO_REFLECTION=1.
#
# This runs in a non-interactive shell when Claude Code fires Stop;
# the prompt + read pair only works if stdin is a TTY. We detect that
# and short-circuit otherwise — the hook becomes a no-op outside an
# interactive context (e.g. CI runs of `claude`).

[ "${SOVEREIGN_NO_REFLECTION:-0}" = "1" ] && exit 0
[ -t 0 ] || exit 0

PORT="${SOVEREIGN_PORT:-9741}"

printf '\n--- session reflection (sovereign) ---\n' >&2
printf 'Record? [d=decision i=invariant t=todo / Enter to skip]: ' >&2
IFS= read -r KIND_CHAR || exit 0
case "$KIND_CHAR" in
    d|D) KIND="decision" ;;
    i|I) KIND="invariant" ;;
    t|T) KIND="todo" ;;
    *) exit 0 ;;
esac
printf 'Content (one line): ' >&2
IFS= read -r CONTENT || exit 0
[ -z "$CONTENT" ] && exit 0

# Escape the content for embedding in JSON. python3 handles tricky
# chars (quotes, backslashes, unicode) without us hand-rolling.
PAYLOAD=$(python3 - "$KIND" "$CONTENT" <<'EOF'
import json, os, sys
kind, content = sys.argv[1], sys.argv[2]
args = {"kind": kind, "content": content, "scope": "global"}
fid = os.environ.get("SOVEREIGN_FEATURE_ID")
if fid:
    args["scope"] = "feature"
    args["feature_id"] = fid
print(json.dumps({
    "jsonrpc": "2.0",
    "id": 1,
    "method": "tools/call",
    "params": {"name": "note", "arguments": args},
}))
EOF
)

curl -sf --max-time 2 \
    -X POST "http://localhost:${PORT}/mcp" \
    -H "Content-Type: application/json" \
    -d "$PAYLOAD" >/dev/null 2>&1 \
    && printf '✓ recorded as %s\n' "$KIND" >&2 \
    || printf '⚠ daemon unreachable; reflection not saved\n' >&2

exit 0
