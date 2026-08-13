#!/bin/bash
# Seat boot block via inject-notes.py (order seat-boot-block).
#
# A seat session — detected by the comaintainer skill marker in its
# transcript, not by an env var — gets ONE pre-assembled rail block on its
# first prompt: anchor todos + recent decisions from related_to=comaintainer-
# seat, open orders, directive-log stats, at a fixed budget. Once per
# session (boot-block.json marker): a second prompt carries no block, and
# the block's notes land in the E2 retrieval stream as one labeled row so
# the audit can score the boot rail. Non-seat sessions never see it, and a
# session that invokes the skill mid-way becomes a seat on the NEXT prompt.
set -u
cd "$(git rev-parse --show-toplevel)" || exit 1

ROOT="$(mktemp -d)"
export SOVEREIGN_SESSIONS_DIR="$ROOT/sessions"
export SVRNMESH_RETRIEVAL_LOG_DIR="$ROOT/retrieval-log"
mkdir -p "$SOVEREIGN_SESSIONS_DIR"
trap 'kill "${STUB_PID:-}" 2>/dev/null; rm -rf "$ROOT"' EXIT

pass=0; fail=0
check() { if [ "$2" = "$3" ]; then echo "  ok   $1"; pass=$((pass+1));
          else echo "  FAIL $1: expected [$2] got [$3]"; fail=$((fail+1)); fi; }

TODO_ID="dddddddd-1111-2222-3333-444444444444"
DEC_ID="eeeeeeee-5555-6666-7777-888888888888"
TODO_BODY="TODO_CLAIM_MARKER seat todo claim line"
DEC_BODY="DECISION_CLAIM_MARKER seat decision claim line"

# ── stub MCP endpoint (serves read_notes AND the block's notes call) ─────────
cat > "$ROOT/stub.py" <<'PYEOF'
import json, os, sys
from http.server import BaseHTTPRequestHandler, HTTPServer
class H(BaseHTTPRequestHandler):
    def do_POST(self):
        self.rfile.read(int(self.headers.get("Content-Length", 0)))
        notes = json.load(open(os.environ["STUB_NOTES"]))
        inner = json.dumps({"notes": notes, "total": len(notes)})
        body = json.dumps({"result": {"content": [{"type": "text", "text": inner}]}}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def log_message(self, *a):
        pass
HTTPServer(("127.0.0.1", int(sys.argv[1])), H).serve_forever()
PYEOF

python3 - "$ROOT" "$TODO_ID" "$DEC_ID" "$TODO_BODY" "$DEC_BODY" <<'PYEOF'
import json, sys
root, todo_id, dec_id, todo_body, dec_body = sys.argv[1:6]
json.dump([
    {"id": todo_id, "kind": "todo", "scope": "global", "content": todo_body},
    {"id": dec_id, "kind": "decision", "scope": "global", "content": dec_body},
], open(f"{root}/notes.json", "w"))
PYEOF

PORT=$(python3 -c "import socket;s=socket.socket();s.bind(('127.0.0.1',0));print(s.getsockname()[1]);s.close()")
STUB_NOTES="$ROOT/notes.json" python3 "$ROOT/stub.py" "$PORT" &
STUB_PID=$!
export SOVEREIGN_PORT="$PORT"
for _ in $(seq 1 50); do
  curl -sf -X POST "http://127.0.0.1:$PORT/mcp" -d '{}' >/dev/null 2>&1 && break
  sleep 0.1
done

HOOK_CMD="${HOOK_CMD:-python3 .claude/hooks/inject-notes.py}"
hook() { printf '{"session_id":"%s","transcript_path":"%s"}' "$1" "$2" | $HOOK_CMD; }

# ── 1. the seat session: block on the first prompt, once ─────────────────────
SID="seat-sess-0001"
mkdir -p "$SOVEREIGN_SESSIONS_DIR/$SID"
# The comaintainer skill marker, serialized the way the transcript does it.
printf '%s\n' '{"type":"user","message":{"content":[{"type":"tool_use","name":"Skill","input":{"skill":"comaintainer"}}]}}' \
  > "$SOVEREIGN_SESSIONS_DIR/$SID/transcript.jsonl"

OUT1="$(hook "$SID" "$SOVEREIGN_SESSIONS_DIR/$SID/transcript.jsonl")"
case "$OUT1" in *"Seat boot block"*) got=yes;; *) got=no;; esac
check "first seat prompt carries the boot block" "yes" "$got"
case "$OUT1" in *"${TODO_ID:0:8}"*"${DEC_ID:0:8}"*) got=yes;; *) got=no;; esac
check "the block indexes the seat-anchor notes (todo + decision)" "yes" "$got"
case "$OUT1" in *"TODO_CLAIM_MARKER"*) got=yes;; *) got=no;; esac
check "…with claim lines, so the titles are useful" "yes" "$got"
case "$OUT1" in *"Open orders"*) got=yes;; *) got=no;; esac
check "the order rail section renders (file read, daemon-independent)" "yes" "$got"
case "$OUT1" in *"Directive log"*) got=yes;; *) got=no;; esac
check "the directive-log rail section renders" "yes" "$got"

MARKER="$SOVEREIGN_SESSIONS_DIR/$SID/boot-block.json"
check "boot-block.json marker written (once-per-session truth)" "yes" "$([ -f "$MARKER" ] && echo yes || echo no)"
REC="$(cat "$MARKER")"
case "$REC" in *"$TODO_ID"*"$DEC_ID"*) got=yes;; *) got=no;; esac
check "the record carries BOTH fixture note ids" "yes" "$got"
case "$REC" in *'"delivered": true'*) got=yes;; *) got=no;; esac
check "…delivered=true (they made the block)" "yes" "$got"
PAYLOAD=$(printf '%s' "$REC" | python3 -c "import json,sys; r=json.load(sys.stdin); print(r['payload_chars'])")
BUDGET=$(printf '%s' "$REC" | python3 -c "import json,sys; r=json.load(sys.stdin); print(r['budget_chars'])")
check "the block stayed inside its budget" "1" "$([ "$PAYLOAD" -le "$BUDGET" ] && echo 1 || echo 0)"

LOG="$SVRNMESH_RETRIEVAL_LOG_DIR/$SID.jsonl"
check "a seat-boot-block retrieval-log row exists" "yes" "$([ -f "$LOG" ] && grep -q '"seat boot block"' "$LOG" && echo yes || echo no)"
ROW="$(grep '"seat boot block"' "$LOG" | tail -1)"
case "$ROW" in *"$TODO_ID"*"$DEC_ID"*) got=yes;; *) got=no;; esac
check "…naming both notes (the audit's denominator)" "yes" "$got"
case "$ROW" in *'"delivered": true'*) got=yes;; *) got=no;; esac
check "…delivered=true (they entered context)" "yes" "$got"
LINES1="$(wc -l < "$LOG" | tr -d ' ')"

# ── 2. once per session: the second prompt carries no block ──────────────────
OUT2="$(hook "$SID" "$SOVEREIGN_SESSIONS_DIR/$SID/transcript.jsonl")"
case "$OUT2" in *"Seat boot block"*) got=yes;; *) got=no;; esac
check "second prompt injects NO block (marker semantics)" "no" "$got"
check "…and logs nothing new" "1" "$([ "$(wc -l < "$LOG" | tr -d ' ')" = "$LINES1" ] && echo 1 || echo 0)"

# ── 3. a mid-session skill invocation becomes a seat on the NEXT prompt ───────
SID3="seat-sess-0002"
mkdir -p "$SOVEREIGN_SESSIONS_DIR/$SID3"
# Transcript starts with ordinary prompts, THEN the skill marker arrives.
printf '%s\n' '{"type":"user","message":{"content":[{"type":"text","text":"hello"}]}}' \
  > "$SOVEREIGN_SESSIONS_DIR/$SID3/transcript.jsonl"
OUT3A="$(hook "$SID3" "$SOVEREIGN_SESSIONS_DIR/$SID3/transcript.jsonl")"
case "$OUT3A" in *"Seat boot block"*) got=yes;; *) got=no;; esac
check "pre-skill prompt: no boot block yet" "no" "$got"
printf '%s\n' '{"type":"user","message":{"content":[{"type":"tool_use","name":"Skill","input":{"skill":"comaintainer"}}]}}' \
  >> "$SOVEREIGN_SESSIONS_DIR/$SID3/transcript.jsonl"
OUT3B="$(hook "$SID3" "$SOVEREIGN_SESSIONS_DIR/$SID3/transcript.jsonl")"
case "$OUT3B" in *"Seat boot block"*) got=yes;; *) got=no;; esac
check "skill invoked mid-session: block fires on the next prompt" "yes" "$got"

# ── 4. non-seat sessions never see the block ─────────────────────────────────
SID4="plain-sess-0001"
mkdir -p "$SOVEREIGN_SESSIONS_DIR/$SID4"
OUT4="$(hook "$SID4" "$SOVEREIGN_SESSIONS_DIR/$SID4/transcript.jsonl")"
case "$OUT4" in *"Seat boot block"*) got=yes;; *) got=no;; esac
check "non-seat session: no block" "no" "$got"
check "…no boot-block marker written" "no" "$([ -f "$SOVEREIGN_SESSIONS_DIR/$SID4/boot-block.json" ] && echo yes || echo no)"
check "…no seat-boot-block log row" "0" "$(grep -c '"seat boot block"' "$SVRNMESH_RETRIEVAL_LOG_DIR/$SID4.jsonl" 2>/dev/null || true)"

echo
echo "  pass: $pass  fail: $fail"
[ "$fail" -eq 0 ]
