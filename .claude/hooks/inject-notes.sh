#!/bin/sh
# sovereign inject-notes — UserPromptSubmit hook for Claude Code.
#
# Injects the notes most RELEVANT to the current prompt from the sovereign
# external brain, instead of a flat recency dump. Reuses the existing MCP
# `read_notes` tool; the only change from the old hook is that it now passes
# the user's prompt as a `query` (semantic when the daemon's embed slot is up),
# dedups, and fails VISIBLY when the daemon is down — so the agent knows the
# brain is dark rather than silently assuming no notes are relevant.
#
# stdin is the UserPromptSubmit payload (JSON with a `prompt` field). We capture
# it before handing the heredoc to python.

export SOVEREIGN_HOOK_INPUT="$(cat)"
export SOVEREIGN_PORT="${SOVEREIGN_PORT:-9741}"

exec python3 - <<'PY' 2>/dev/null
import os, json, hashlib, urllib.request

port = os.environ.get("SOVEREIGN_PORT", "9741")
try:
    payload = json.loads(os.environ.get("SOVEREIGN_HOOK_INPUT") or "{}")
except Exception:
    payload = {}
prompt = (payload.get("prompt") or "").strip()

# Relevance-ranked when we have a prompt; recency fallback otherwise.
args = {"kinds": ["invariant", "decision"], "limit": 8}
query = prompt[:400]
if query:
    args["query"] = query
    args["semantic"] = True

body = json.dumps({
    "jsonrpc": "2.0", "id": 1, "method": "tools/call",
    "params": {"name": "read_notes", "arguments": args},
}).encode()
req = urllib.request.Request(
    f"http://localhost:{port}/mcp", data=body,
    headers={"content-type": "application/json"},
)
try:
    with urllib.request.urlopen(req, timeout=3) as r:
        outer = json.loads(r.read().decode())
    notes = json.loads(outer["result"]["content"][0]["text"]).get("notes", [])
except Exception:
    # Fail VISIBLE, not silent — the agent should know the brain is unreachable.
    print("_(sovereign external brain unavailable — daemon down; notes not injected)_")
    raise SystemExit(0)

# Dedup by content (the store can return the same global note more than once).
seen, uniq = set(), []
for n in notes:
    c = (n.get("content") or "").strip()
    if not c:
        continue
    h = hashlib.sha256(c.encode()).hexdigest()
    if h in seen:
        continue
    seen.add(h)
    uniq.append(n)

if not uniq:
    raise SystemExit(0)

scope = "most relevant to your prompt" if query else "active"
print(f"## Sovereign notes ({scope}, injected by hook)\n")
for n in uniq:
    print(f"[{n.get('kind', 'note')}] {n.get('content', '').strip()}\n")
PY
