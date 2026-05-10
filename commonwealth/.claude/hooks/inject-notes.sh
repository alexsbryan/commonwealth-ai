#!/bin/sh
# sovereign inject-notes — UserPromptSubmit hook for Claude Code.
# Fetches active invariants and decisions from the sovereign MCP server and
# prints them as context before every Claude response.
# Fails silently when the server is not running so offline work is unaffected.

PORT="${SOVEREIGN_PORT:-9741}"

# ATOS scope-aware payload. When $SOVEREIGN_FEATURE_ID is set (by
# `sovereign atos start-milestone`), the query pulls global notes plus
# the active feature's notes. Otherwise only globals are injected.
if [ -n "${SOVEREIGN_FEATURE_ID:-}" ]; then
  PAYLOAD=$(printf '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"read_notes","arguments":{"kinds":["invariant","decision"],"scope":["global","feature"],"feature_id":"%s","limit":20}}}' "$SOVEREIGN_FEATURE_ID")
else
  PAYLOAD='{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"read_notes","arguments":{"kinds":["invariant","decision"],"scope":["global"],"limit":20}}}'
fi

RESPONSE=$(curl -sf --max-time 2 \
  -X POST "http://localhost:${PORT}/mcp" \
  -H "Content-Type: application/json" \
  -d "$PAYLOAD" \
  2>/dev/null) || exit 0

[ -z "$RESPONSE" ] && exit 0

printf '%s' "$RESPONSE" | python3 -c "
import sys, os, json

try:
    outer = json.load(sys.stdin)
    inner_text = outer['result']['content'][0]['text']
    inner = json.loads(inner_text)
    notes = inner.get('notes', [])
    if not notes:
        sys.exit(0)
    fid = os.environ.get('SOVEREIGN_FEATURE_ID', '')
    header = '## Active sovereign notes (injected by hook)'
    if fid:
        header = header + ' (feature=' + fid + ')'
    print(header)
    print()
    for n in notes:
        kind = n.get('kind', 'note')
        scope = n.get('scope', 'global')
        content = n.get('content', '').strip()
        tag = kind if scope == 'global' else kind + '/' + scope
        print('[' + tag + '] ' + content)
        print()
except Exception:
    sys.exit(0)
" 2>/dev/null
