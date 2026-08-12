#!/usr/bin/env bash
# co-mesh-drill.sh — two-seat conformance drill for order seat-durable-rail.
#
# Proves UC-D1..D4 across two machines (RuggedFox and BeefyMac), each
# running its own side of the drill. Prior art: the five-channel
# mesh-seat-coordination-drill (closed as landed 2026-08-11) — per-case
# entry points, four-verdict readings, operator-relayed cross-machine
# steps. Full procedure: scripts/CO_MESH_DRILL.md.
#
#   scripts/co-mesh-drill.sh open <id> <title>            # D1 side B (writer)
#   scripts/co-mesh-drill.sh list-check <id> <node-tag>   # D1 side A (reader)
#   scripts/co-mesh-drill.sh close <id>                   # D1 side B
#   scripts/co-mesh-drill.sh gone-check <id>              # D1 side A
#   scripts/co-mesh-drill.sh note <marker> <content>      # D2 side A (writer)
#   scripts/co-mesh-drill.sh ambient-check <marker>       # D2 side B (reader)
#   scripts/co-mesh-drill.sh reply <marker> <content>     # D2 side B (writer)
#   scripts/co-mesh-drill.sh seen-check <marker>          # D2 side A (reader)
#   scripts/co-mesh-drill.sh directive <marker>           # D3 side A: pending+resolve
#   scripts/co-mesh-drill.sh stats-check <marker> <side>  # D3 either: store has it,
#                                                         # attributed; prints --stats
#   scripts/co-mesh-drill.sh d4-check ordinary|seat       # D4 either machine
#   scripts/co-mesh-drill.sh cleanup <marker>             # retire the drill's notes
#   scripts/co-mesh-drill.sh report <a.log> <b.log>       # four-verdict table
#
# FOUR VERDICTS, not two (ARCH §18.2): passed / failed /
# could-not-judge / never-ran. A step that cannot tell — daemon down,
# gossip link silent, relay unavailable — says could-not-judge WITH the
# observed evidence; it never pretends to pass or fail.
#
# Every step prints a DRILL_STEP line; a side run's stdout is the side
# log the other machine's `report` reads (operator-relayed).
#
# TIME BUDGET: <5 minutes per case. Bounded polls only — a step that
# does not see its expected state inside the window prints
# could-not-judge with what it DID see.
set -uo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
PY="$(command -v python3 || echo python3)"
CO_DIR="$(cd "$(dirname "$0")" && pwd)"
# The D3 drill rows go to a DRILL-SPECIFIC local log, so re-running the
# drill never pollutes the real M0 metric. The mesh write-through goes
# to the real notes store either way — the store is what the
# cross-machine comparison reads.
DRILL_LOG="$HOME/.sovereign/comaintainer/directives.drill.jsonl"
DRILL_ANNOT="$HOME/.sovereign/comaintainer/directive-edit-verdicts.drill.jsonl"
export CO_DIRECTIVE_LOG="$DRILL_LOG"
export CO_DIRECTIVE_ANNOTATIONS="$DRILL_ANNOT"

DAEMON_UP() { "$PY" "$CO_DIR/co_notes.py" read-notes --limit 1 >/dev/null 2>&1; }

# poll_until <label> <max_s> <cmd...> — run the check every 5s until it
# exits 0 or the budget is spent. Prints the elapsed seconds on success.
poll_until() {
  local label="$1" max="$2"; shift 2
  local t=0
  while [ "$t" -le "$max" ]; do
    if "$@" >/dev/null 2>&1; then
      echo "  ($label ok after ${t}s)" >&2
      return 0
    fi
    sleep 5; t=$((t+5))
  done
  return 1
}

step() { # step <case> <step> <PASS|FAIL|could-not-judge> <detail...>
  local c="$1" s="$2" r="$3"; shift 3
  printf 'DRILL_STEP %s %s %s %s\n' "$c" "$s" "$r" "$*"
}

case "$1" in
  open) # D1 side B: open a real order (file + mesh shadow note)
    ID="${2:?usage: open <id> <title>}"
    TITLE="${3:-$ID}"
    DAEMON_UP || { step D1 open could-not-judge "daemon unreachable on this side"; exit 0; }
    "$REPO/scripts/co-order.sh" new "$ID" "$TITLE"
    step D1 open PASS "order $ID drafted with mesh shadow"
    ;;
  list-check) # D1 side A: poll until co-order.sh list shows it, attributed
    ID="${2:?usage: list-check <id> <node-tag>}"
    TAG="${3:-}"
    DAEMON_UP || { step D1 list-check could-not-judge "daemon unreachable — cannot list mesh rows"; exit 0; }
    if poll_until "list shows $ID" 90 \
        bash -c "'$REPO/scripts/co-order.sh' list | grep -q '$ID'" 2>/dev/null; then
      LINE="$("$REPO/scripts/co-order.sh" list | grep "$ID" | head -1)"
      if [ -n "$TAG" ] && ! grep -q "\[$TAG" <<<"$LINE"; then
        step D1 list-check FAIL "order $ID listed but WITHOUT attribution $TAG — got: $LINE"
      else
        step D1 list-check PASS "order $ID listed with attribution — $LINE"
      fi
    else
      step D1 list-check could-not-judge "order $ID not listed inside 90s (gossip/relay window) — see co-order.sh list output"
    fi
    ;;
  close) # D1 side B: close the order; the shadow note is retired
    ID="${2:?usage: close <id>}"
    "$REPO/scripts/co-order.sh" close "$ID"
    step D1 close PASS "order $ID file closed; mesh note retired if it existed"
    ;;
  gone-check) # D1 side A: poll until the id is gone from `list`
    ID="${2:?usage: gone-check <id>}"
    DAEMON_UP || { step D1 gone-check could-not-judge "daemon unreachable — cannot list mesh rows"; exit 0; }
    if poll_until "gone: $ID" 90 \
        bash -c "! '$REPO/scripts/co-order.sh' list | grep -q '$ID'" 2>/dev/null; then
      step D1 gone-check PASS "order $ID no longer listed (tombstone converged)"
    else
      step D1 gone-check could-not-judge "order $ID still listed after 90s — local file may remain (file is truth)"
    fi
    ;;
  note) # D2 side A: unanchored global decision note (ambient-carryable)
    MARKER="${2:?usage: note <marker> <content>}"
    CONTENT="${3:-coordination drill note}"
    DAEMON_UP || { step D2 note could-not-judge "daemon unreachable on this side"; exit 0; }
    OUT="$(CO_DIR="$CO_DIR" "$PY" "$CO_DIR/co_notes.py" write-note --kind decision \
        --scope global --content "drill: $MARKER
$CONTENT")" || { step D2 note FAIL "write-note failed: $OUT"; exit 0; }
    NID="$(printf '%s' "$OUT" | "$PY" -c 'import json,sys; print((json.load(sys.stdin).get("id") or "")[:8])')"
    step D2 note PASS "note $NID written (marker $MARKER)"
    ;;
  ambient-check) # D2 side B: the PROMPT HOOK's ambient path carries it
    MARKER="${2:?usage: ambient-check <marker>}"
    DAEMON_UP || { step D2 ambient-check could-not-judge "daemon unreachable — no ambient read possible"; exit 0; }
    POLL_FN() {
      printf '{"session_id":"co-mesh-drill-%s"}' "$MARKER" | \
        SOVEREIGN_PORT="${SOVEREIGN_PORT:-9741}" "$PY" "$REPO/.claude/hooks/inject-notes.py" 2>/dev/null | \
        grep -q "drill: $MARKER"
    }
    if poll_until "ambient carries $MARKER" 90 POLL_FN; then
      OUT="$(printf '{"session_id":"co-mesh-drill-%s"}' "$MARKER" | \
        SOVEREIGN_PORT="${SOVEREIGN_PORT:-9741}" "$PY" "$REPO/.claude/hooks/inject-notes.py" 2>/dev/null | \
        grep "drill: $MARKER" | head -1)"
      step D2 ambient-check PASS "hook ambient carried the note — $OUT"
    else
      step D2 ambient-check could-not-judge "note not in ambient inside 90s — gossip link may be down or not yet converged (the D2 BOOTSTRAP: a seat sees only what has already gossiped)"
    fi
    ;;
  reply) # D2 side B: reply to the marker (unanchored note)
    MARKER="${2:?usage: reply <marker> <content>}"
    CONTENT="${3:-reply from side B}"
    DAEMON_UP || { step D2 reply could-not-judge "daemon unreachable on this side"; exit 0; }
    OUT="$(CO_DIR="$CO_DIR" "$PY" "$CO_DIR/co_notes.py" write-note --kind decision \
        --scope global --content "reply-to: $MARKER
$CONTENT")" || { step D2 reply FAIL "write-note failed: $OUT"; exit 0; }
    NID="$(printf '%s' "$OUT" | "$PY" -c 'import json,sys; print((json.load(sys.stdin).get("id") or "")[:8])')"
    step D2 reply PASS "reply note $NID written (reply-to $MARKER)"
    ;;
  seen-check) # D2 side A: the reply gossips back and shows in an ordinary read
    MARKER="${2:?usage: seen-check <marker>}"
    DAEMON_UP || { step D2 seen-check could-not-judge "daemon unreachable on this side"; exit 0; }
    POLL_FN() {
      "$PY" "$CO_DIR/co_notes.py" read-notes --limit 100 2>/dev/null | grep -q "reply-to: $MARKER"
    }
    if poll_until "reply to $MARKER visible" 90 POLL_FN; then
      step D2 seen-check PASS "reply to $MARKER visible from an ORDINARY read (no include_operational)"
    else
      step D2 seen-check could-not-judge "reply not visible after 90s — gossip/relay window exceeded"
    fi
    ;;
  directive) # D3 side A: pending + resolve through the REAL script path
    MARKER="${2:?usage: directive <marker>}"
    DAEMON_UP || { step D3 directive could-not-judge "daemon unreachable on this side"; exit 0; }
    DID="$("$REPO/scripts/co-directive-log.sh" --pending --kind decision \
        --draft "drill $MARKER" 2>"$DRILL_LOG.err")"
    if [ -z "$DID" ]; then
      step D3 directive FAIL "no directive id from --pending: $(cat "$DRILL_LOG.err")"
      exit 0
    fi
    OUT="$("$REPO/scripts/co-directive-log.sh" --resolve "$DID" --final "drill $MARKER resolved" --unedited 2>&1)"
    step D3 directive PASS "directive $DID pending+resolved (unedited) — $OUT"
    ;;
  stats-check) # D3 either side: store has the drill row, attributed; prints --stats
    MARKER="${2:?usage: stats-check <marker> <side>}"
    SIDE="${3:-?}"
    DAEMON_UP || { step D3 stats-check could-not-judge "daemon unreachable — local tally only"; exit 0; }
    ROW="$(CO_DIR="$CO_DIR" "$PY" "$CO_DIR/co_notes.py" read-notes --include-operational \
        --kinds decision --limit 100 2>/dev/null | MARKER="$MARKER" "$PY" -c '
import json, os, sys
env = json.load(sys.stdin)
hits = []
for n in env.get("notes", []):
    if "drill %s" % os.environ["MARKER"] in (n.get("content") or ""):
        hits.append((n.get("author") or "unknown origin",
                     (n.get("id") or "")[:8]))
for author, nid in hits:
    print(f"{author}\t{nid}")
')"
    STATS="$(CO_DIRECTIVE_LOG="$DRILL_LOG" CO_DIRECTIVE_ANNOTATIONS="$DRILL_ANNOT" \
        "$REPO/scripts/co-directive-log.sh" --stats 2>"$DRILL_LOG.err")"
    ALL_ROW="$(printf '%s\n' "$STATS" | grep '^ALL ' | head -1)"
    BANNER="$(grep -c 'LOCAL tally' "$DRILL_LOG.err" || true)"
    if [ -n "$ROW" ]; then
      if printf '%s\n' "$ROW" | grep -qv "unknown origin"; then
        step D3 stats-check PASS "store row for $MARKER attributed — $(printf '%s' "$ROW" | head -1); ALL row: $ALL_ROW"
      else
        step D3 stats-check FAIL "store row for $MARKER present but UNATTRIBUTED: $ROW"
      fi
    else
      step D3 stats-check could-not-judge "no store row for $MARKER (mesh write-through may have failed); ALL row: $ALL_ROW"
    fi
    printf 'DRILL_TABLE %s %s\n' "$SIDE" "$ALL_ROW"
    [ "$BANNER" = 0 ] || printf 'DRILL_TABLE %s banner "LOCAL tally — daemon fallback"\n' "$SIDE"
    ;;
  d4-check) # D4: the flood guard, both directions
    MODE="${2:?usage: d4-check ordinary|seat}"
    DAEMON_UP || { step D4 "$MODE" could-not-judge "daemon unreachable"; exit 0; }
    SID="co-mesh-drill-D4-$(date +%s)"
    if [ "$MODE" = ordinary ]; then
      OUT="$(printf '{"session_id":"%s"}' "$SID" | SOVEREIGN_PORT="${SOVEREIGN_PORT:-9741}" \
        "$PY" "$REPO/.claude/hooks/inject-notes.py" 2>/dev/null)"
      WITHHELD="$(printf '%s\n' "$OUT" | grep -c 'operational record(s) withheld' || true)"
      LEAKED="$(printf '%s\n' "$OUT" | grep -v 'withheld' | grep -c 'order-seat\|directive-log' || true)"
      if [ "$WITHHELD" -ge 1 ] && [ "$LEAKED" = 0 ]; then
        step D4 ordinary PASS "withheld line present (${WITHHELD}x); zero anchored records leaked"
      elif [ "$WITHHELD" -eq 0 ]; then
        step D4 ordinary could-not-judge "no withheld line — daemon may be pre-registry, or no anchored records exist yet"
      else
        step D4 ordinary FAIL "anchored records leaked into an ordinary session: $LEAKED line(s)"
      fi
    else
      OUT="$(printf '{"session_id":"%s"}' "$SID" | SOVEREIGN_SEAT=1 SOVEREIGN_PORT="${SOVEREIGN_PORT:-9741}" \
        "$PY" "$REPO/.claude/hooks/inject-notes.py" 2>/dev/null)"
      WITHHELD="$(printf '%s\n' "$OUT" | grep -c 'operational record(s) withheld' || true)"
      # The anchored-order summary carries "order: drill-…" in its first
      # (indexed) line — the anchored-presence proof. Directive rows show
      # only "directive: <key>" in the index; their anchoring is proven by
      # D3's stats-check instead.
      CARRIED="$(printf '%s\n' "$OUT" | grep -c 'order: drill-' || true)"
      if [ "$WITHHELD" = 0 ] && [ "$CARRIED" -ge 1 ]; then
        step D4 seat PASS "no withheld line; seat ambient carried $CARRIED anchored record(s)"
      elif [ "$CARRIED" = 0 ]; then
        step D4 seat could-not-judge "no anchored drill records in the store yet — run D1 first, or the mesh link is down"
      else
        step D4 seat FAIL "withheld line still present in a seat session ($WITHHELD)"
      fi
    fi
    ;;
  cleanup) # retire every note the drill wrote for this marker
    MARKER="${2:?usage: cleanup <marker>}"
    DAEMON_UP || { echo "co-mesh-drill: daemon unreachable — nothing retired"; exit 0; }
    # --include-operational: the drill's anchored rows (order note,
    # directive notes) are withheld from ordinary reads — cleanup must
    # ask for the seat rail to see them.
    "$PY" "$CO_DIR/co_notes.py" read-notes --include-operational --limit 100 2>/dev/null | \
      MARKER="$MARKER" CO_DIR="$CO_DIR" "$PY" -c '
import json, os, sys
sys.path.insert(0, os.environ["CO_DIR"])
from co_notes import retire_note
env = json.load(sys.stdin)
gone = 0
marker = os.environ["MARKER"]
for n in env.get("notes", []):
    c = n.get("content") or ""
    if marker not in c:
        continue
    is_drill = (c.startswith("drill:") or c.startswith("reply-to:")
                or c.startswith("order: drill") or c.startswith("directive:")
                or "draft: drill " in c or "final: drill " in c)
    if is_drill:
        try:
            retire_note(n["id"], f"co-mesh-drill cleanup ({marker})")
            gone += 1
        except Exception as exc:
            print(f"could-not-retire {n['id']}: {exc}", file=sys.stderr)
print(f"co-mesh-drill: retired {gone} drill note(s) for {marker}")
'
    [ -e "$DRILL_LOG" ] && rm -f "$DRILL_LOG" "$DRILL_ANNOT" "$DRILL_LOG.err"
    echo "co-mesh-drill: drill local log cleared"
    ;;
  report) # assemble the four-verdict table from the two side logs
    A="${2:?usage: report <side-a.log> <side-b.log>}"
    B="${3:?}"
    "$PY" - "$A" "$B" <<'PY'
import re, sys
from collections import defaultdict
steps = defaultdict(list)   # case -> [(step, result, detail)]
tables = []
for path in sys.argv[1:3]:
    for line in open(path, encoding="utf-8", errors="replace"):
        line = line.strip()
        if line.startswith("DRILL_STEP"):
            _, c, s, r, *rest = line.split(" ", 4)
            steps[c].append((s, r, " ".join(rest)))
        elif line.startswith("DRILL_TABLE"):
            # (side, row) — the side label is not part of the number, so
            # the cross-machine comparison compares rows only.
            tables.append(line.split(" ", 2)[1:])

CASES = ["D1", "D2", "D3", "D4"]
print(f"{'case':<6} {'verdict':<16} evidence")
for c in CASES:
    rows = steps.get(c, [])
    if not rows:
        print(f"{c:<6} never-ran        no steps on either side")
        continue
    if any(r == "FAIL" for _, r, _ in rows):
        print(f"{c:<6} failed           {sum(1 for _, r, _ in rows if r == 'FAIL')} FAIL step(s): "
              + "; ".join(f"{s} {d}" for s, r, d in rows if r == "FAIL"))
        continue
    if any(r != "PASS" for _, r, _ in rows):
        print(f"{c:<6} could-not-judge  {sum(1 for _, r, _ in rows if r != 'PASS')} step(s) not PASS: "
              + "; ".join(f"{s}: {d}" for s, r, d in rows if r != "PASS"))
        continue
    if c == "D3" and len(tables) >= 2:
        by_side = dict(tables)
        a_row, b_row = by_side.get("A", ""), by_side.get("B", "")
        if a_row.startswith("banner") or b_row.startswith("banner"):
            print(f"{c:<6} could-not-judge  one side fell back to the LOCAL tally: "
                  f"A={a_row[:40]} B={b_row[:40]}")
            continue
        if a_row != b_row:
            print(f"{c:<6} could-not-judge  ALL rows differ across machines — "
                  f"A: {a_row.strip()} vs B: {b_row.strip()}")
            continue
    print(f"{c:<6} passed           "
          + "; ".join(f"{s} ({r})" for s, r, _ in rows))
if tables:
    print()
    print("D3 ALL rows relayed:")
    for side, t in tables:
        print(f"  {side}: {t}")
print()
print("Verdicts are four, not two (ARCH §18.2): passed / failed /")
print("could-not-judge (evidence recorded) / never-ran (not invoked).")
PY
    ;;
  *) echo "usage: co-mesh-drill.sh open|list-check|close|gone-check|note|ambient-check|reply|seen-check|directive|stats-check|d4-check|cleanup|report ..." >&2
     exit 2 ;;
esac
