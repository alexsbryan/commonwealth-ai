#!/bin/bash
# End-to-end through the REAL UserPromptSubmit notes hook, against an isolated
# sessions store and a stub MCP endpoint.
#
# What this guards. The shell version this replaced printed all 20 note bodies
# on EVERY prompt — 61,664 bytes (~15.4k tokens) per injection, registered
# twice, re-billed as cache-read on every later turn. The payload was so large
# it was not read: a session re-derived a median that was sitting in a note the
# frame had explicitly told it to open. Each check below is one of the three
# properties that failure needs.
set -u
cd "$(git rev-parse --show-toplevel)" || exit 1

ROOT="$(mktemp -d)"
export SOVEREIGN_SESSIONS_DIR="$ROOT/sessions"
# Isolate the E2 retrieval stream — the hook must never write test rows into
# the live fleet baseline (order seat-boot-block).
export SVRNMESH_RETRIEVAL_LOG_DIR="$ROOT/retrieval-log"
mkdir -p "$SOVEREIGN_SESSIONS_DIR"
trap 'kill "${STUB_PID:-}" 2>/dev/null; rm -rf "$ROOT"' EXIT

pass=0; fail=0
check() { if [ "$2" = "$3" ]; then echo "  ok   $1"; pass=$((pass+1));
          else echo "  FAIL $1: expected [$2] got [$3]"; fail=$((fail+1)); fi; }

CITED_ID="aaaaaaaa-1111-2222-3333-444444444444"
PLAIN_ID="bbbbbbbb-5555-6666-7777-888888888888"
LATE_ID="cccccccc-9999-0000-1111-222222222222"
CITED_BODY="CITED_BODY_MARKER the predecessor said read this"
# Deliberately multi-line: the index shows a note's CLAIM LINE, so a fixture
# whose body is one short line cannot distinguish "summarised" from "dumped".
# PLAIN_TAIL is the part that must never reach the turn.
PLAIN_HEAD="PLAIN_HEAD_MARKER ordinary note nobody cited"
PLAIN_TAIL="PLAIN_TAIL_MARKER the body that must stay out of context"
PLAIN_BODY="$PLAIN_HEAD
$PLAIN_TAIL"
LATE_BODY="LATE_BODY_MARKER written mid-session"

# ── stub MCP endpoint ────────────────────────────────────────────────────────
# Serves the read_notes envelope the hook parses. Reads which notes to serve
# from a file so the test can add one mid-session.
cat > "$ROOT/stub.py" <<'PYEOF'
import json, os, sys
from http.server import BaseHTTPRequestHandler, HTTPServer
NOTES_FILE = os.environ["STUB_NOTES"]
class H(BaseHTTPRequestHandler):
    def do_POST(self):
        self.rfile.read(int(self.headers.get("Content-Length", 0)))
        with open(NOTES_FILE) as fh:
            notes = json.load(fh)
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

python3 - "$ROOT" "$CITED_ID" "$PLAIN_ID" "$CITED_BODY" "$PLAIN_BODY" <<'PYEOF'
import json, sys
root, cited_id, plain_id, cited_body, plain_body = sys.argv[1:6]
json.dump([
    {"id": cited_id, "kind": "decision", "scope": "global", "content": cited_body},
    {"id": plain_id, "kind": "invariant", "scope": "global", "content": plain_body},
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

SID="test-session-0001"
mkdir -p "$SOVEREIGN_SESSIONS_DIR/$SID"
# A frame that cites ONE note by its 8-char prefix, the way prose actually does.
printf -- '---\nsession_id: %s\nstatus: in-flight\n---\n\n## State\n\nCITATIONS IN NOTE %s — read it, do not re-derive.\n' \
  "$SID" "${CITED_ID:0:8}" > "$SOVEREIGN_SESSIONS_DIR/$SID/frame.md"

# Overridable so the same checks can be run against a candidate hook — that is
# how these were watched RED against the shell version they replaced
# (ARCH_PRINCIPLES §18.1: a gate you have not watched fail is not a gate).
HOOK_CMD="${HOOK_CMD:-python3 .claude/hooks/inject-notes.py}"
hook() { printf '{"session_id":"%s"}' "$1" | $HOOK_CMD; }

# ── 1. first prompt ──────────────────────────────────────────────────────────
OUT1="$(hook "$SID")"
case "$OUT1" in *"$CITED_BODY"*) got=yes;; *) got=no;; esac
check "frame-cited note is inlined IN FULL" "yes" "$got"

case "$OUT1" in *"$PLAIN_TAIL"*) got=yes;; *) got=no;; esac
check "non-cited note BODY is NOT dumped" "no" "$got"

case "$OUT1" in *"${PLAIN_ID:0:8}"*) got=yes;; *) got=no;; esac
check "non-cited note appears as an index line" "yes" "$got"

case "$OUT1" in *"$PLAIN_HEAD"*) got=yes;; *) got=no;; esac
check "...carrying its claim line, so the title is useful" "yes" "$got"

# ── 1b. the E2 retrieval stream (restored 2026-08-13, order seat-boot-block) ─
# One record per prompt, `delivered` set AFTER the budget — the honest
# denominator `sovereign notes retrieval-audit` reads.
LOG="$SVRNMESH_RETRIEVAL_LOG_DIR/$SID.jsonl"
check "E2 log row written on the first prompt" "yes" "$([ -f "$LOG" ] && echo yes || echo no)"
ROW1="$(tail -1 "$LOG")"
case "$ROW1" in *"$CITED_ID"*"$PLAIN_ID"*) got=yes;; *) got=no;; esac
check "the row names BOTH injected notes" "yes" "$got"
case "$ROW1" in *'"delivered": true'*) got=yes;; *) got=no;; esac
check "both marked delivered (they entered context)" "yes" "$got"
case "$ROW1" in *'"symbols"'*'"terms"'*) got=yes;; *) got=no;; esac
check "the row carries symbols + terms (the audit's match surface)" "yes" "$got"

# ── 2. dedupe ────────────────────────────────────────────────────────────────
OUT2="$(hook "$SID")"
check "second prompt injects NOTHING (already surfaced)" "0" "$(printf '%s' "$OUT2" | wc -c | tr -d ' ')"
check "second prompt logs NOTHING (no fresh notes)" "1" "$(wc -l < "$LOG" | tr -d ' ')"

# ── 3. a note written mid-session still gets through ─────────────────────────
python3 - "$ROOT" "$LATE_ID" "$LATE_BODY" <<'PYEOF'
import json, sys
root, late_id, late_body = sys.argv[1:4]
p = f"{root}/notes.json"
notes = json.load(open(p))
notes.insert(0, {"id": late_id, "kind": "decision", "scope": "global", "content": late_body})
json.dump(notes, open(p, "w"))
PYEOF
OUT3="$(hook "$SID")"
case "$OUT3" in *"${LATE_ID:0:8}"*) got=yes;; *) got=no;; esac
check "a NEW note still reaches a later prompt" "yes" "$got"
case "$OUT3" in *"${PLAIN_ID:0:8}"*) got=yes;; *) got=no;; esac
check "...without re-sending the ones already surfaced" "no" "$got"

# ── 3b. the seat sidecar: detection is published for the frame writer ────────
# The hook is the only surface holding a transcript_path, so it is the only
# thing that can see the comaintainer skill marker. It hands that fact to
# `session_state` (which stamps `role: seat` into the frame) through a file —
# and the successor's "take the seat" line depends on that file existing.
seat_hook() { printf '{"session_id":"%s","transcript_path":"%s"}' "$1" "$2" | $HOOK_CMD; }
role_of() { cat "$SOVEREIGN_SESSIONS_DIR/$1/role" 2>/dev/null | tr -d '\n'; }
present() { [ -f "$SOVEREIGN_SESSIONS_DIR/$1/role" ] && echo present || echo absent; }

# BOTH ways a session takes the seat. The Skill tool call is what the detector
# knew; the slash command is how the live seat 53a08260 actually took its seat
# on 2026-08-13, and missing it shielded that seat from its own rail all day.
printf '{"type":"user","message":"hi"}\n{"skill": "comaintainer"}\n' > "$ROOT/seat-tool.jsonl"
printf '{"type":"user","message":{"role":"user","content":"<command-message>comaintainer</command-message>\\n<command-name>/comaintainer</command-name>"}}\n' \
  > "$ROOT/seat-slash.jsonl"
printf '{"type":"user","message":"hi"}\n{"type":"assistant","message":"ok"}\n' > "$ROOT/worker.jsonl"
# The skill LISTING is attached to EVERY session, workers included. If it
# counted, every session on the box would be a seat.
printf '{"type":"user","attachment":{"type":"skill_listing","content":"- comaintainer: Take the comaintainer director seat — the operator%s primary interface."}}\n' "'s" \
  > "$ROOT/listing-only.jsonl"

seat_hook "seat-tool-01"   "$ROOT/seat-tool.jsonl"    >/dev/null 2>&1
seat_hook "seat-slash-01"  "$ROOT/seat-slash.jsonl"   >/dev/null 2>&1
seat_hook "worker-01"      "$ROOT/worker.jsonl"       >/dev/null 2>&1
seat_hook "listing-01"     "$ROOT/listing-only.jsonl" >/dev/null 2>&1

check "seat via the Skill tool publishes its role sidecar"  "seat"   "$(role_of seat-tool-01)"
check "seat via /comaintainer publishes it too"             "seat"   "$(role_of seat-slash-01)"
check "a worker session publishes nothing"                  "absent" "$(present worker-01)"
check "the skill LISTING alone is not a seat"               "absent" "$(present listing-01)"

# ── 4. the hook must never block a prompt ────────────────────────────────────
kill "$STUB_PID" 2>/dev/null; wait "$STUB_PID" 2>/dev/null
OUT4="$(hook "test-session-0002"; echo "rc=$?")"
check "daemon down: silent, exit 0" "rc=0" "$OUT4"

echo
echo "  pass: $pass  fail: $fail"
[ "$fail" -eq 0 ]
