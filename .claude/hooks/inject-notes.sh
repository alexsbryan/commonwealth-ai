#!/bin/sh
# sovereign inject-notes — UserPromptSubmit hook for Claude Code.
# Fetches active invariants and decisions from the sovereign MCP server and
# prints them as context before every Claude response.
# Fails silently when the server is not running so offline work is unaffected.

PORT="${SOVEREIGN_PORT:-9741}"

RESPONSE=$(curl -sf --max-time 2 \
  -X POST "http://localhost:${PORT}/mcp" \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"read_notes","arguments":{"kinds":["invariant","decision"],"limit":20}}}' \
  2>/dev/null) || exit 0

[ -z "$RESPONSE" ] && exit 0

printf '%s' "$RESPONSE" | python3 -c "
import sys, json

try:
    outer = json.load(sys.stdin)
    inner_text = outer['result']['content'][0]['text']
    inner = json.loads(inner_text)
    notes = inner.get('notes', [])
    if not notes:
        sys.exit(0)
    print('## Active sovereign notes (injected by hook)')
    print()
    for n in notes:
        kind = n.get('kind', 'note')
        content = n.get('content', '').strip()
        print('[' + kind + '] ' + content)
        print()
except Exception:
    sys.exit(0)
" 2>/dev/null
