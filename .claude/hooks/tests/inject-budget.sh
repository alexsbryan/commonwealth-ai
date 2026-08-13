#!/bin/bash
# First-prompt budget enforcement in inject-notes.py (MEMORY_MODEL §5 E5,
# restored 2026-08-13 by order seat-boot-block).
#
# The frame-bearing first prompt is capped at 3200 chars (spec: notes ≤3200).
# The hook caps each cited BODY at NOTE_MAX_CHARS (2000) first; when even the
# capped body cannot fit, overflow degrades to a dereferenceable pointer (P1),
# never silent truncation: the dropped note is NAMED, and the E2 record
# carries the delivered flag set AFTER the budget, so the audit's denominator
# stays honest. This suite watches the two failure shapes: a cited body whose
# capped form blows the budget, and a budget so small nothing fits.
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

BIG_ID="aaaaaaaa-1111-2222-3333-444444444444"
PLAIN_ID="bbbbbbbb-5555-6666-7777-888888888888"
BIG_HEAD="BIG_BODY_HEAD_MARKER the frame cited this note"
BIG_TAIL="BIG_BODY_TAIL_MARKER must never enter context at this budget"
# 5000 chars — over the 3200 first-prompt budget even with the index lines.
BIG_BODY="$(python3 -c "
print('$BIG_HEAD'); print('x'*4900); print('$BIG_TAIL')")"

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

python3 - "$ROOT" "$BIG_ID" "$PLAIN_ID" "$BIG_BODY" <<'PYEOF'
import json, sys
root, big_id, plain_id, big_body = sys.argv[1:5]
json.dump([
    {"id": big_id, "kind": "decision", "scope": "global", "content": big_body},
    {"id": plain_id, "kind": "invariant", "scope": "global", "content": "PLAIN note body"},
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
hook() { printf '{"session_id":"%s"}' "$1" | $HOOK_CMD; }

SID="budget-sess-0001"
mkdir -p "$SOVEREIGN_SESSIONS_DIR/$SID"
printf -- '---\nsession_id: %s\n---\n\n## State\n\nCITATIONS IN NOTE %s — read it.\n' \
  "$SID" "${BIG_ID:0:8}" > "$SOVEREIGN_SESSIONS_DIR/$SID/frame.md"

# The hook caps each cited body at NOTE_MAX_CHARS (2000) BEFORE budget
# fitting, so a 5000-char body becomes ~2000 and fits a 3200 budget. The
# pointer degradation fires when the CAPPED body exceeds the budget — so the
# fixture squeezes the first-prompt budget to 1500 (< 2000) and expects the
# pointer line, with the body never entering context.
OUT1="$(SOVEREIGN_FIRST_PROMPT_NOTES_BUDGET=1500 hook "$SID")"

# The pointer's dereference hint is a SEMANTIC read on the note's distinctive
# terms (there is no exact-id route on the daemon surface — `notes(query: "<id>")`
# returns notes that merely mention the id; `svrn notes list --id` reads the
# repo-local store). The fixture body's first distinctive term is the
# BIG_BODY_HEAD_MARKER token, and the id itself must NOT appear as a query.
case "$OUT1" in *"body at \`notes(query: \"BIG_BODY_HEAD_MARKER"*) got=yes;; *) got=no;; esac
check "over-budget cited body degrades to a dereference pointer" "yes" "$got"
case "$OUT1" in *"notes(query=\"${BIG_ID:0:8}\")"*) got=yes;; *) got=no;; esac
check "...and the pointer dereferences by terms, never by the id" "no" "$got"
case "$OUT1" in *"$BIG_TAIL"*) got=yes;; *) got=no;; esac
check "...and the body NEVER enters context" "no" "$got"
case "$OUT1" in *"${PLAIN_ID:0:8}"*) got=yes;; *) got=no;; esac
check "the index lines still land (they are pointers)" "yes" "$got"

LOG="$SVRNMESH_RETRIEVAL_LOG_DIR/$SID.jsonl"
ROW="$(tail -1 "$LOG")"
case "$ROW" in *"$BIG_ID"*'"delivered": true'*'"truncated": true'*) got=yes;; *) got=no;; esac
check "E2 record: cited note delivered=true truncated=true (pointer reached context)" "yes" "$got"

# ── a budget so small nothing fits: absence is named, never defaulted ────────
SID2="budget-sess-0002"
mkdir -p "$SOVEREIGN_SESSIONS_DIR/$SID2"
printf -- '---\nsession_id: %s\n---\n\n## State\n\nCITATIONS IN NOTE %s — read it.\n' \
  "$SID2" "${BIG_ID:0:8}" > "$SOVEREIGN_SESSIONS_DIR/$SID2/frame.md"
OUT2="$(SOVEREIGN_FIRST_PROMPT_NOTES_BUDGET=100 hook "$SID2")"
case "$OUT2" in *"spent before cited notes"*) got=yes;; *) got=no;; esac
check "tiny budget: the overflow is NAMED, not silently dropped" "yes" "$got"
ROW2="$(tail -1 "$SVRNMESH_RETRIEVAL_LOG_DIR/$SID2.jsonl")"
case "$ROW2" in *'"delivered": false'*) got=yes;; *) got=no;; esac
check "E2 record: the dropped note is delivered=false (honest denominator)" "yes" "$got"

echo
echo "  pass: $pass  fail: $fail"
[ "$fail" -eq 0 ]
