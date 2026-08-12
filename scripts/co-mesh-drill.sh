#!/usr/bin/env bash
# co-mesh-drill.sh — two-seat conformance drill for order seat-durable-rail
# (UC-D1..D4) AND order commons-fluency (UC-F1..F8), same script.
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
# Commons-fluency F-drill (order commons-fluency, UC-F1..F8) — the SAME
# script, extended. Each F-case is a verb, the two seats run their whole
# side with one call, and the verdict table is assembled from the notes
# alone (UC-F8 — the drill runs itself; no operator relay after the
# one-time start note).
#
#   scripts/co-mesh-drill.sh f-start <run-id> [peer-node]  # F8: write the start note
#   scripts/co-mesh-drill.sh f-exec <run-id>               # F8: THIS side's whole run
#   scripts/co-mesh-drill.sh f-assemble <run-id>           # F8: verdicts from notes alone
#   scripts/co-mesh-drill.sh f1-take <run-id> <label> [--ttl N]   # F1 writer (also F2/F3)
#   scripts/co-mesh-drill.sh f1-release <run-id> <label>   # F1 hygiene: release own claim
#   scripts/co-mesh-drill.sh f1-sight <run-id> <label> <peer-node>  # F1 reader ≤92s
#   scripts/co-mesh-drill.sh f2-expired <run-id> <label> <peer-node> # F2 tombstone window
#   scripts/co-mesh-drill.sh f3-note <run-id> <label>      # F3 writer (origin side)
#   scripts/co-mesh-drill.sh f3-origin <run-id> <label>    # F3 origin receipt (sent_at)
#   scripts/co-mesh-drill.sh f3-receipt <run-id> <label>   # F3 peer receipt (received_at)
#   scripts/co-mesh-drill.sh f3-claim <run-id> <label>     # F3 claims-rail receipt
#   scripts/co-mesh-drill.sh f3-negative <run-id> <label>  # F3 NEGATIVE ARM (sent_at null)
#   scripts/co-mesh-drill.sh f3-liveness <run-id>          # F3 LIVENESS ARM (/status)
#   scripts/co-mesh-drill.sh f4-note <run-id> <label>      # F4 addressed note (writer)
#   scripts/co-mesh-drill.sh f4-ambient <run-id> <label>   # F4 peer ambient (SOVEREIGN_SEAT)
#   scripts/co-mesh-drill.sh f4-reply <run-id> <label>     # F4 reply (writer)
#   scripts/co-mesh-drill.sh f4-seen <run-id> <label>      # F4 reply round-trip (reader)
#   scripts/co-mesh-drill.sh f5-check <run-id>             # F5/F6 the flood gate + named
#                                                          # withholding (extends D4)
#   scripts/co-mesh-drill.sh f7-wire <run-id>              # F7 zero-bucket wire forms
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

# The F-verbs (f-*) carry the run id in $2; the step() printer then
# writes every F-verdict as an anchored note too — UC-F8 assembles the
# verdict table from the notes alone. The D-verbs leave DRILL_RUN empty
# and stay operator-relayed via the stdout log.
case "$1" in f-*) DRILL_RUN="${2:-}";; *) DRILL_RUN="";; esac

step() { # step <case> <step> <PASS|FAIL|could-not-judge> <detail...>
  local c="$1" s="$2" r="$3"; shift 3
  printf 'DRILL_STEP %s %s %s %s\n' "$c" "$s" "$r" "$*"
  if [ -n "$DRILL_RUN" ]; then
    "$PY" "$CO_DIR/co_notes.py" write-note --kind decision --scope global \
      --related-entity order-seat \
      --content "DRILL_STEP $c $s $r $* run=$DRILL_RUN" >/dev/null 2>&1 || true
  fi
}

# ── Commons-fluency F-drill helpers (order commons-fluency UC-F1..F8) ───
# Every F-case verdict is printed AND written as an anchored note; the
# verdict table is assembled from the notes alone (UC-F8).
CLAIM_BIN="$(command -v svrn 2>/dev/null || command -v sovereign 2>/dev/null || echo sovereign)"

# claim <args...> — the claim CLI (svrn on prod hosts, sovereign elsewhere).
claim() { "$CLAIM_BIN" claim "$@"; }

# own_node_tag — THIS node's display form (node-<16hex>) from /status.
own_node_tag() {
  curl -s --max-time 5 "http://127.0.0.1:${SOVEREIGN_PORT:-9741}/status" 2>/dev/null | \
    "$PY" -c 'import json,sys
try: print(json.load(sys.stdin).get("node_id") or "")
except Exception: print("")'
}

# wait_until <epoch-unix> — sleep until the absolute time (never past).
wait_until() {
  local d=$(( $1 - $(date +%s) ))
  [ "$d" -gt 0 ] && sleep "$d"
}

# run_notes <run-id> — the run's notes (start + step notes), as JSON.
run_notes() {
  "$PY" "$CO_DIR/co_notes.py" read-notes --include-operational --kinds decision \
    --limit 200 2>/dev/null | RUN="$1" "$PY" -c '
import json, os, sys
env = json.load(sys.stdin)
run = os.environ["RUN"]
out = [n for n in env.get("notes", []) if run in (n.get("content") or "")]
print(json.dumps({"notes": out}))
'
}

# start_note <run-id> — the run's start note JSON (first match).
start_note() {
  run_notes "$1" | RUN="$1" "$PY" -c '
import json, os, sys
env = json.load(sys.stdin)
run = os.environ["RUN"]
for n in env.get("notes", []):
    if (n.get("content") or "").startswith("drill-start: " + run):
        print(json.dumps(n)); break
'
}

# note_field <note-json> <field> — one `field: value` line of the note.
note_field() {
  printf '%s' "$1" | F="$2" "$PY" -c '
import json, os, sys
f = os.environ["F"]
for line in json.load(sys.stdin).get("content", "").splitlines():
    if line.startswith(f + ": "):
        print(line[len(f) + 2:]); break
'
}

# f_note <run-id> <first-line-prefix> — the note whose content starts with
# the prefix, as `sent=.. recv=.. created=..` field values. Stamps are
# unix seconds in the row (created_at is RFC3339 in the payload — the
# helper normalizes everything to unix so the drill can bracket with
# integer arithmetic).
f_note() {
  run_notes "$1" | MARK="$2" "$PY" -c '
import json, os, sys
from datetime import datetime, timezone
env = json.load(sys.stdin)
mark = os.environ["MARK"]
def to_unix(v):
    if v is None or v == "":
        return ""
    s = str(v)
    try:
        return str(int(s))
    except (TypeError, ValueError):
        pass
    try:
        return str(int(datetime.fromisoformat(s.replace("Z", "+00:00")).timestamp()))
    except Exception:
        return s
for n in env.get("notes", []):
    if (n.get("content") or "").startswith(mark):
        print("sent=%s recv=%s created=%s" % (
            to_unix(n.get("sent_at")), to_unix(n.get("received_at")),
            to_unix(n.get("created_at")))); break
'
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
      WITHHELD="$(printf '%s\n' "$OUT" | grep -c 'operational record(s) withheld (anchored to' || true)"
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
      WITHHELD="$(printf '%s\n' "$OUT" | grep -c 'operational record(s) withheld (anchored to' || true)"
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
  f-start) # F8 bootstrap: write the start note the drill runs from.
    RUN="${2:?usage: f-start <run-id> [peer-node-tag]}"
    PEER="${3:-unknown}"
    DAEMON_UP || { step F8 start could-not-judge "daemon unreachable on this side"; exit 0; }
    SELF_TAG="$(own_node_tag)"
    [ -n "$SELF_TAG" ] || { step F8 start could-not-judge "no node tag from /status — daemon predates the wire form or is unreachable"; exit 0; }
    EPOCH="$(date +%s)"
    OUT="$("$PY" "$CO_DIR/co_notes.py" write-note --kind decision --scope global \
        --related-entity order-seat \
        --content "drill-start: $RUN
epoch: $EPOCH
side-a: $SELF_TAG
side-b: $PEER
cases: F1 F2 F3 F4 F5 F6 F7 F8")" || { step F8 start FAIL "start note write failed: $OUT"; exit 0; }
    NID="$(printf '%s' "$OUT" | "$PY" -c 'import json,sys; print((json.load(sys.stdin).get("id") or "")[:8])')"
    step F8 start PASS "start note $NID written at epoch $EPOCH (this node $SELF_TAG, peer $PEER) — the bootstrap honesty clause: the channel cannot carry an instruction to a session that is not yet watching"
    ;;
  f1-take) # F1/F2 writer: take the drill claim (F1 long-TTL for the sighting, F2 short-TTL for the abandonment).
    RUN="${2:?usage: f1-take <run-id> <label> [ttl-seconds]}"
    LABEL="${3:-F1-self}"
    TTL="${4:-600}"
    CASE="${LABEL%%-*}"
    DAEMON_UP || { step "$CASE" take could-not-judge "daemon unreachable on this side"; exit 0; }
    OUT="$(claim take "drill:$RUN:$LABEL" --intent "drill $RUN $LABEL" --ttl "$TTL" 2>&1)" || {
      step "$CASE" take FAIL "claim take failed: $OUT"; exit 0; }
    CID="$(printf '%s\n' "$OUT" | sed -n 's/^  id:       //p' | head -1)"
    MAY="$(claim may-i "drill:$RUN:$LABEL" 2>&1)"
    if printf '%s\n' "$MAY" | grep -q "node: ?"; then
      step "$CASE" take could-not-judge "origin's own claim reads node: ? — daemon predates fix 1 (node_id in ClaimRecord): $MAY"
    elif printf '%s\n' "$MAY" | grep -q "held"; then
      step "$CASE" take PASS "claim $CID taken on drill:$RUN:$LABEL (TTL ${TTL}s), origin reads held with its node"
    else
      step "$CASE" take FAIL "origin read not held after take: $MAY"
    fi
    ;;
  f1-release) # F1 hygiene: release this side's own F1 claim (the F2 claims expire by design).
    RUN="${2:?usage: f1-release <run-id> <label>}"
    LABEL="${3:-F1-self}"
    CID="$(claim may-i "drill:$RUN:$LABEL" --format json 2>/dev/null | "$PY" -c '
import json, sys
try:
    d = json.load(sys.stdin)
    print(d["claims"][0]["claim_id"] if d.get("claims") else "")
except Exception: print("")')"
    [ -n "$CID" ] && claim release "$CID" >/dev/null 2>&1
    step F1 release PASS "claim $LABEL released (drill hygiene; F2 claims age out via TTL)"
    ;;
  f1-sight) # F1 reader: first sighting of the peer's claim ≤92s, WITH the peer's node id.
    RUN="${2:?usage: f1-sight <run-id> <label> <peer-node>}"
    LABEL="${3:-F1-peer}"
    PEER="${4:-}"
    DAEMON_UP || { step F1 sight could-not-judge "daemon unreachable on this side"; exit 0; }
    T0="$(date +%s)"
    SIGHT=""
    while [ $(( $(date +%s) - T0 )) -le 92 ]; do
      OUT="$(claim may-i "drill:$RUN:$LABEL" 2>&1)"
      if printf '%s\n' "$OUT" | grep -q "held"; then SIGHT="$OUT"; break; fi
      sleep 5
    done
    if [ -z "$SIGHT" ]; then
      step F1 sight could-not-judge "peer claim drill:$RUN:$LABEL never read held inside 92s (gossip link or peer down — TTL 600 gave ~500s of margin)"
      exit 0
    fi
    ELAPSED=$(( $(date +%s) - T0 ))
    if printf '%s\n' "$SIGHT" | grep -q "node: ?"; then
      step F1 sight FAIL "first sighting read held WITHOUT a node id — fix 1 regression: $(printf '%s' "$SIGHT" | head -3 | tr '\n' ' ')"
    elif [ -n "$PEER" ] && ! printf '%s\n' "$SIGHT" | grep -q "node: $PEER"; then
      step F1 sight FAIL "first sighting names the wrong node (expected $PEER): $(printf '%s' "$SIGHT" | grep 'node:' | head -1)"
    else
      step F1 sight PASS "first sighting held WITH node id inside ${ELAPSED}s (≤92s bound)"
    fi
    ;;
  f2-expired) # F2: the peer's abandoned claim reads expired — and STAYS expired (tombstoned, fix 2) across a GC sweep, distinct from free.
    RUN="${2:?usage: f2-expired <run-id> <label> <peer-node>}"
    LABEL="${3:-F2-peer}"
    PEER="${4:-}"
    DAEMON_UP || { step F2 expired could-not-judge "daemon unreachable on this side"; exit 0; }
    T0="$(date +%s)"
    FIRST=""
    while [ $(( $(date +%s) - T0 )) -le 90 ]; do
      OUT="$(claim may-i "drill:$RUN:$LABEL" 2>&1)"
      if printf '%s\n' "$OUT" | grep -q "expired"; then FIRST="$OUT"; break; fi
      if printf '%s\n' "$OUT" | grep -q "free"; then
        step F2 expired FAIL "peer claim reads FREE before its expired reading — the eviction collapse fix 2 exists to prevent: $OUT"
        exit 0
      fi
      sleep 5
    done
    [ -n "$FIRST" ] || { step F2 expired could-not-judge "claim drill:$RUN:$LABEL never read expired inside 90s (peer claim missing — peer down?)"; exit 0; }
    FIRST_T=$(( $(date +%s) - T0 ))
    FIRST_LINE="$(printf '%s\n' "$FIRST" | head -1)"
    # Second sample ≥60s later — a full GC sweep cadence must pass
    # without the expired verdict collapsing into free.
    sleep 60
    SECOND="$(claim may-i "drill:$RUN:$LABEL" 2>&1)"
    if printf '%s\n' "$SECOND" | grep -q "free"; then
      step F2 expired FAIL "expired reading COLLAPSED to free after a GC sweep: first '$FIRST_LINE' → '$SECOND'"
    elif printf '%s\n' "$SECOND" | grep -q "abandoned"; then
      step F2 expired PASS "expired at +${FIRST_T}s ('$FIRST_LINE'), persisted across a GC sweep — still expired+abandoned, distinct from free"
    elif printf '%s\n' "$SECOND" | grep -q "expired"; then
      step F2 expired could-not-judge "still expired but without the abandonment wording — tombstone render may predate the fix: '$SECOND'"
    else
      step F2 expired could-not-judge "second sample unexpected: '$SECOND'"
    fi
    ;;
  f3-note) # F3 writer: the receipt marker note (origin side).
    RUN="${2:?usage: f3-note <run-id> <label>}"
    LABEL="${3:-mark}"
    DAEMON_UP || { step F3 note could-not-judge "daemon unreachable on this side"; exit 0; }
    OUT="$("$PY" "$CO_DIR/co_notes.py" write-note --kind decision --scope global \
        --related-entity order-seat --content "drill: $RUN-$LABEL
receipt marker $LABEL for run $RUN")" || { step F3 note FAIL "write failed: $OUT"; exit 0; }
    NID="$(printf '%s' "$OUT" | "$PY" -c 'import json,sys; print((json.load(sys.stdin).get("id") or "")[:8])')"
    step F3 note PASS "marker note $NID written (drill: $RUN-$LABEL)"
    ;;
  f3-origin) # F3 origin receipt: the ORIGIN's row carries sent_at (publish fired); received_at stays null (never self-ingested).
    RUN="${2:?usage: f3-origin <run-id> <label>}"
    LABEL="${3:-mark}"
    DAEMON_UP || { step F3 origin could-not-judge "daemon unreachable on this side"; exit 0; }
    T0="$(date +%s)"
    RES=""
    while [ $(( $(date +%s) - T0 )) -le 60 ]; do
      RES="$(f_note "$RUN" "drill: $RUN-$LABEL")"
      SENT="$(printf '%s' "$RES" | sed -n 's/^sent=//p' | cut -d' ' -f1)"
      [ -n "$SENT" ] && break
      sleep 5
    done
    SENT="$(printf '%s' "$RES" | sed -n 's/^sent=//p' | cut -d' ' -f1)"
    RECV="$(printf '%s' "$RES" | sed -n 's/.*recv=//p' | cut -d' ' -f1)"
    if [ -n "$SENT" ] && [ -z "$RECV" ]; then
      step F3 origin PASS "origin row carries sent_at $SENT, received_at null — the publish fired, on record"
    elif [ -z "$SENT" ]; then
      step F3 origin FAIL "origin row has NO sent_at — the publish never fired (the 981dd6d8 failure mode, now diagnosable from the origin alone): $RES"
    else
      step F3 origin FAIL "origin row carries received_at $RECV — unexpected for a local write: $RES"
    fi
    ;;
  f3-receipt) # F3 peer receipt: the PEER's row carries received_at ≥ created_at (≥ sent_at); the round-trip is a bracket from stamps.
    RUN="${2:?usage: f3-receipt <run-id> <label>}"
    LABEL="${3:-mark}"
    DAEMON_UP || { step F3 receipt could-not-judge "daemon unreachable on this side"; exit 0; }
    T0="$(date +%s)"
    RES=""
    while [ $(( $(date +%s) - T0 )) -le 90 ]; do
      RES="$(f_note "$RUN" "drill: $RUN-$LABEL")"
      RECV="$(printf '%s' "$RES" | sed -n 's/.*recv=//p' | cut -d' ' -f1)"
      [ -n "$RECV" ] && break
      sleep 5
    done
    RECV="$(printf '%s' "$RES" | sed -n 's/.*recv=//p' | cut -d' ' -f1)"
    SENT="$(printf '%s' "$RES" | sed -n 's/^sent=//p' | cut -d' ' -f1)"
    CREATED="$(printf '%s' "$RES" | sed -n 's/.*created=//p')"
    if [ -z "$RECV" ]; then
      step F3 receipt could-not-judge "peer row for $RUN-$LABEL has no received_at inside 90s (gossip link silent?)"
    elif [ -z "$CREATED" ]; then
      step F3 receipt could-not-judge "peer row carries received_at $RECV but no created_at to bracket against: $RES"
    elif [ "$RECV" -ge "$CREATED" ] 2>/dev/null && { [ -z "$SENT" ] || [ "$RECV" -ge "$SENT" ]; } 2>/dev/null; then
      BRACKET=$(( RECV - CREATED ))
      step F3 receipt PASS "round-trip bracket ${BRACKET}s — created $CREATED → received $RECV (sent_at $SENT); computable from stamps alone"
    else
      step F3 receipt FAIL "receipt violates stamp ordering: $RES"
    fi
    ;;
  f3-claim) # F3 claims-rail receipt: the PEER store stamped received_at when it applied the peer's claim.
    RUN="${2:?usage: f3-claim <run-id> <label>}"
    LABEL="${3:-F1-peer}"
    DAEMON_UP || { step F3 claim could-not-judge "daemon unreachable on this side"; exit 0; }
    OUT="$(claim may-i "drill:$RUN:$LABEL" --format json 2>&1)" || {
      step F3 claim could-not-judge "may-i unavailable: $OUT"; exit 0; }
    RECV="$(printf '%s' "$OUT" | "$PY" -c '
import json, sys
try:
    d = json.load(sys.stdin)
    claims = d.get("claims") or []
    if not claims:
        print("none")
    else:
        print(claims[0].get("received_at") if claims[0].get("received_at") is not None else "null")
except Exception: print("")')"
    case "$RECV" in
      none) step F3 claim could-not-judge "no claim row for drill:$RUN:$LABEL — peer claim never applied here (gossip link?)";;
      null) step F3 claim FAIL "claim applied but received_at is null — fix 3b (claims-rail receipt) missing";;
      "") step F3 claim could-not-judge "may-i payload unparseable";;
      *) step F3 claim PASS "peer store stamped received_at $RECV on the applied claim";;
    esac
    ;;
  f3-negative) # F3 NEGATIVE ARM: a session-scoped write never fires the publish sink → sent_at null on the origin.
    RUN="${2:?usage: f3-negative <run-id> <label>}"
    LABEL="${3:-neg}"
    DAEMON_UP || { step F3 negative could-not-judge "daemon unreachable on this side"; exit 0; }
    OUT="$("$PY" "$CO_DIR/co_notes.py" write-note --kind decision --scope session \
        --content "drill-neg: $RUN-$LABEL
negative arm: a write whose publish never fires")" || { step F3 negative FAIL "write failed: $OUT"; exit 0; }
    NID="$(printf '%s' "$OUT" | "$PY" -c 'import json,sys; print((json.load(sys.stdin).get("id") or "")[:8])')"
    RES="$(f_note "$RUN" "drill-neg: $RUN-$LABEL")"
    SENT="$(printf '%s' "$RES" | sed -n 's/^sent=//p' | cut -d' ' -f1)"
    RECV="$(printf '%s' "$RES" | sed -n 's/.*recv=//p' | cut -d' ' -f1)"
    if [ -z "$SENT" ] && [ -z "$RECV" ]; then
      step F3 negative PASS "session-scoped write $NID reads sent_at: null on the origin — the unpublishable failure mode is diagnosable from the origin alone"
    elif [ -n "$SENT" ]; then
      step F3 negative FAIL "session-scoped write carries sent_at $SENT — the publish path fired for a note it must not publish"
    else
      step F3 negative could-not-judge "negative-arm row unreadable: $RES"
    fi
    ;;
  f3-liveness) # F3 LIVENESS ARM: /status exposes the publish-path convergence age as BRACKETS + a stale flag (fix 9).
    RUN="${2:?usage: f3-liveness <run-id>}"
    STATUS="$(curl -s --max-time 5 "http://127.0.0.1:${SOVEREIGN_PORT:-9741}/status" 2>/dev/null)"
    [ -n "$STATUS" ] || { step F3 liveness could-not-judge "/status unreachable"; exit 0; }
    LINE="$(printf '%s' "$STATUS" | "$PY" -c '
import json, sys
try: d = json.load(sys.stdin)
except Exception: print("unparseable"); raise SystemExit
c = d.get("convergence")
if not c: print("absent"); raise SystemExit
out = c.get("outbound_publish") or {}
inn = c.get("inbound_ingest") or {}
print("out_bucket=%s out_stale=%s in_bucket=%s in_stale=%s" % (
    out.get("age_bucket") or "", out.get("stale"), inn.get("age_bucket") or "", inn.get("stale")))
')"
    case "$LINE" in
      absent) step F3 liveness could-not-judge "/status has no convergence section — daemon predates fix 9";;
      unparseable) step F3 liveness could-not-judge "status JSON unparseable";;
      out_bucket=*)
        OUT_B="$(printf '%s' "$LINE" | sed -n 's/.*out_bucket=//p' | cut -d' ' -f1)"
        case "$OUT_B" in
          0-30s|30s-2m|2-5m|5-30m|">30m"|never)
            step F3 liveness PASS "convergence age readable on /status as BRACKETS, not points: $LINE (a silent publish path would read stale=true — never vs stale are distinct)"
            ;;
          *) step F3 liveness FAIL "age_bucket is not a bracket: $LINE";;
        esac
        ;;
      *) step F3 liveness could-not-judge "$LINE";;
    esac
    ;;
  f4-note) # F4 writer: the addressed coordination note (anchored order-seat).
    RUN="${2:?usage: f4-note <run-id> <label>}"
    LABEL="${3:-f4}"
    DAEMON_UP || { step F4 note could-not-judge "daemon unreachable on this side"; exit 0; }
    OUT="$("$PY" "$CO_DIR/co_notes.py" write-note --kind decision --scope global \
        --related-entity order-seat --content "drill: $RUN-$LABEL
addressed seat-to-seat coordination note (UC-F4)")" || { step F4 note FAIL "write failed: $OUT"; exit 0; }
    NID="$(printf '%s' "$OUT" | "$PY" -c 'import json,sys; print((json.load(sys.stdin).get("id") or "")[:8])')"
    step F4 note PASS "addressed note $NID written (drill: $RUN-$LABEL)"
    ;;
  f4-ambient) # F4 reader: the PEER's addressed note surfaces in THIS seat's ambient (SOVEREIGN_SEAT=1 path).
    RUN="${2:?usage: f4-ambient <run-id> <label>}"
    LABEL="${3:-f4}"
    DAEMON_UP || { step F4 ambient could-not-judge "daemon unreachable — no ambient read possible"; exit 0; }
    POLL_FN() {
      printf '{"session_id":"co-mesh-drill-%s"}' "$RUN" | \
        SOVEREIGN_SEAT=1 SOVEREIGN_PORT="${SOVEREIGN_PORT:-9741}" "$PY" "$REPO/.claude/hooks/inject-notes.py" 2>/dev/null | \
        grep -q "drill: $RUN-$LABEL"
    }
    if poll_until "ambient carries $RUN-$LABEL" 90 POLL_FN; then
      step F4 ambient PASS "peer's addressed note surfaced in the seat ambient (SOVEREIGN_SEAT path, no operator relay)"
    else
      step F4 ambient could-not-judge "addressed note not in ambient inside 90s (gossip link down?)"
    fi
    ;;
  f4-reply) # F4 peer: the reply through the same rail; origin receipt checked on this side.
    RUN="${2:?usage: f4-reply <run-id> <label>}"
    LABEL="${3:-f4}"
    DAEMON_UP || { step F4 reply could-not-judge "daemon unreachable on this side"; exit 0; }
    OUT="$("$PY" "$CO_DIR/co_notes.py" write-note --kind decision --scope global \
        --related-entity order-seat --content "reply-to: $RUN-$LABEL
reply from the peer seat (UC-F4)")" || { step F4 reply FAIL "write failed: $OUT"; exit 0; }
    NID="$(printf '%s' "$OUT" | "$PY" -c 'import json,sys; print((json.load(sys.stdin).get("id") or "")[:8])')"
    SENT="$(f_note "$RUN" "reply-to: $RUN-$LABEL" | sed -n 's/^sent=//p' | cut -d' ' -f1)"
    if [ -n "$SENT" ]; then
      step F4 reply PASS "reply $NID written; origin receipt sent_at $SENT on record"
    else
      step F4 reply FAIL "reply $NID written but origin carries no sent_at: $SENT"
    fi
    ;;
  f4-seen) # F4 origin: the peer's reply round-trips with receipts at both ends.
    RUN="${2:?usage: f4-seen <run-id> <label>}"
    LABEL="${3:-f4}"
    DAEMON_UP || { step F4 seen could-not-judge "daemon unreachable on this side"; exit 0; }
    T0="$(date +%s)"
    RES=""
    while [ $(( $(date +%s) - T0 )) -le 90 ]; do
      RES="$(f_note "$RUN" "reply-to: $RUN-$LABEL")"
      RECV="$(printf '%s' "$RES" | sed -n 's/.*recv=//p' | cut -d' ' -f1)"
      [ -n "$RECV" ] && break
      sleep 5
    done
    RECV="$(printf '%s' "$RES" | sed -n 's/.*recv=//p' | cut -d' ' -f1)"
    SENT="$(printf '%s' "$RES" | sed -n 's/^sent=//p' | cut -d' ' -f1)"
    CREATED="$(printf '%s' "$RES" | sed -n 's/.*created=//p')"
    if [ -z "$RECV" ]; then
      step F4 seen could-not-judge "peer's reply not visible with a receipt inside 90s (gossip link down?)"
    elif [ -z "$CREATED" ]; then
      step F4 seen could-not-judge "reply row carries received_at $RECV but no created_at to bracket against: $RES"
    elif [ "$RECV" -ge "$CREATED" ] 2>/dev/null && [ "$RECV" -ge "$SENT" ] 2>/dev/null; then
      BRACKET=$(( RECV - CREATED ))
      step F4 seen PASS "reply round-tripped with receipts at both ends — bracket ${BRACKET}s (sent_at $SENT → received_at $RECV)"
    else
      step F4 seen FAIL "reply receipt violates stamp ordering: $RES"
    fi
    ;;
  f5-check) # F5/F6: the flood gate still holds — ordinary ambient has ZERO seat records and NAMES the withholding; seat ambient carries the rail.
    RUN="${2:?usage: f5-check <run-id>}"
    ORD="$(printf '{"session_id":"co-mesh-drill-%s"}' "$RUN" | SOVEREIGN_PORT="${SOVEREIGN_PORT:-9741}" \
      "$PY" "$REPO/.claude/hooks/inject-notes.py" 2>/dev/null)"
    WITHHELD="$(printf '%s\n' "$ORD" | grep -c 'operational record(s) withheld (anchored to' || true)"
    LEAKED="$(printf '%s\n' "$ORD" | grep -v 'withheld' | grep -c 'order-seat\|directive-log\|comaintainer-seat' || true)"
    if [ "$WITHHELD" -ge 1 ] && [ "$LEAKED" = 0 ]; then
      HINT="$(printf '%s\n' "$ORD" | grep 'withheld' | head -1)"
      step F5 ordinary PASS "ordinary ambient: zero seat records leaked; the withheld line NAMES anchors and count"
      step F6 withholding PASS "the reader can say what it was not shown: $HINT"
    elif [ "$WITHHELD" -eq 0 ]; then
      step F5 ordinary could-not-judge "no withheld line — daemon pre-registry, or no anchored records on this store"
    else
      step F5 ordinary FAIL "anchored records leaked into an ordinary session: $LEAKED line(s)"
    fi
    SEAT="$(printf '{"session_id":"co-mesh-drill-%s"}' "$RUN" | SOVEREIGN_SEAT=1 SOVEREIGN_PORT="${SOVEREIGN_PORT:-9741}" \
      "$PY" "$REPO/.claude/hooks/inject-notes.py" 2>/dev/null)"
    CARRIED="$(printf '%s\n' "$SEAT" | grep -c "drill: $RUN-f4-" || true)"
    WITHHELD_S="$(printf '%s\n' "$SEAT" | grep -c 'operational record(s) withheld (anchored to' || true)"
    if [ "$WITHHELD_S" = 0 ] && [ "$CARRIED" -ge 1 ]; then
      step F5 seat PASS "seat ambient carried the addressed rail record ($CARRIED), no withheld line"
    else
      step F5 seat could-not-judge "seat ambient did not carry the drill record (or a withheld line appeared: $WITHHELD_S) — run F4 first"
    fi
    ;;
  f7-wire) # F7: the zero-bucket tally row names the rejected header value and the expected 32-hex form.
    RUN="${2:?usage: f7-wire <run-id>}"
    GARBAGE="drill-${RUN}-not-32hex"
    curl -s --max-time 5 -H "X-Node-Id: $GARBAGE" "http://127.0.0.1:${SOVEREIGN_PORT:-9741}/status" >/dev/null 2>&1
    STATUS="$(curl -s --max-time 5 "http://127.0.0.1:${SOVEREIGN_PORT:-9741}/status" 2>/dev/null)"
    [ -n "$STATUS" ] || { step F7 wire could-not-judge "/status unreachable"; exit 0; }
    RES="$(printf '%s' "$STATUS" | GARBAGE="$GARBAGE" "$PY" -c '
import json, os, sys
try: d = json.load(sys.stdin)
except Exception: print("unparseable"); raise SystemExit
garbage = os.environ["GARBAGE"]
for row in (d.get("inference") or {}).get("peer_requests") or []:
    if row.get("node_id") == "node-0000000000000000":
        print("rejected=%s expected=%s" % (row.get("rejected_header_value") or "", row.get("expected_wire_form") or ""))
        break
else:
    print("no-zero-row")
')"
    case "$RES" in
      no-zero-row) step F7 wire could-not-judge "no zero-bucket tally row — daemon predates fix 7";;
      unparseable) step F7 wire could-not-judge "status JSON unparseable";;
      rejected=*)
        REJ="$(printf '%s' "$RES" | sed -n 's/^rejected=//p' | cut -d' ' -f1)"
        EXP="$(printf '%s' "$RES" | sed -n 's/.*expected=//p')"
        if [ "$REJ" = "$GARBAGE" ] && [ -n "$EXP" ]; then
          step F7 wire PASS "zero-bucket row names the rejected header ($REJ) and the expected 32-hex form ($EXP)"
        else
          step F7 wire FAIL "zero-bucket row present but does not name the rejection: $RES"
        fi
        ;;
      *) step F7 wire could-not-judge "$RES";;
    esac
    ;;
  f-exec) # F8: THIS side's whole assignment, on the start note's epoch schedule.
    RUN="${2:?usage: f-exec <run-id>}"
    DAEMON_UP || { step F8 exec could-not-judge "daemon unreachable on this side"; exit 0; }
    START="$(start_note "$RUN")"
    [ -n "$START" ] || { step F8 exec could-not-judge "start note not readable on this side (gossip link down?)"; exit 0; }
    EPOCH="$(note_field "$START" epoch)"
    SIDE_A="$(note_field "$START" side-a)"
    SIDE_B="$(note_field "$START" side-b)"
    SELF="$(own_node_tag)"
    if [ -z "$SELF" ]; then
      step F8 exec could-not-judge "no node tag from /status"; exit 0
    fi
    if [ "$SELF" = "$SIDE_A" ]; then PEER="$SIDE_B"
    elif [ "$SELF" = "$SIDE_B" ]; then PEER="$SIDE_A"
    else
      step F8 exec could-not-judge "this node ($SELF) is neither side of the start note (side-a=$SIDE_A side-b=$SIDE_B)"; exit 0
    fi
    SHORT="${SELF#node-}"; SHORT="${SHORT:0:8}"
    PSHORT="${PEER#node-}"; PSHORT="${PSHORT:0:8}"
    step F8 exec PASS "side $SELF (short $SHORT) running $RUN against peer $PEER; epoch $EPOCH"
    # The watch mechanism the drill runs from (UC-F8): the start note
    # must be surfaced by `seat watch` — the fix-8 verb.
    PROBE_OK=""
    for _ in $(seq 1 18); do
      if "$CLAIM_BIN" seat watch --once 2>&1 | grep -q "drill-start: $RUN"; then PROBE_OK=1; break; fi
      sleep 5
    done
    if [ -n "$PROBE_OK" ]; then
      step F8 watch-probe PASS "seat watch surfaced the start note (fix 8 mechanism live)"
    else
      step F8 watch-probe could-not-judge "seat watch --once did not surface the start note inside 90s (verb missing — pre-deploy? — or note not local)"
    fi
    # 1. writer acts (both sides take their own claims + write their markers)
    wait_until $(( EPOCH + 5 ))
    "$0" f1-take "$RUN" "F1-$SHORT" 600
    "$0" f1-take "$RUN" "F2-$SHORT" 60
    "$0" f3-note "$RUN" "mark-$SHORT"
    "$0" f4-note "$RUN" "f4-$SHORT"
    # 2. reader acts (peer claims + markers + addressed note, gossiped by now)
    wait_until $(( EPOCH + 45 ))
    "$0" f1-sight "$RUN" "F1-$PSHORT" "$PEER"
    "$0" f3-receipt "$RUN" "mark-$PSHORT"
    "$0" f3-claim "$RUN" "F1-$PSHORT"
    "$0" f4-ambient "$RUN" "f4-$PSHORT"
    # 3. the reply round-trips
    wait_until $(( EPOCH + 75 ))
    "$0" f4-reply "$RUN" "f4-$SHORT"
    wait_until $(( EPOCH + 105 ))
    "$0" f4-seen "$RUN" "f4-$PSHORT"
    # 4. the expired tombstone window (peer's abandoned F2 claim)
    wait_until $(( EPOCH + 135 ))
    "$0" f2-expired "$RUN" "F2-$PSHORT" "$PEER"
    # 5. origin receipts, the negative + liveness arms, the flood gate, wire forms
    wait_until $(( EPOCH + 180 ))
    "$0" f1-release "$RUN" "F1-$SHORT"
    "$0" f3-origin "$RUN" "mark-$SHORT"
    "$0" f3-negative "$RUN" "neg-$SHORT"
    "$0" f3-liveness "$RUN"
    "$0" f5-check "$RUN"
    "$0" f7-wire "$RUN"
    # 6. assemble the verdict table from the notes alone
    wait_until $(( EPOCH + 300 ))
    "$0" f-assemble "$RUN"
    ;;
  f-assemble) # F8: assemble the four-verdict table from the run's notes alone.
    RUN="${2:?usage: f-assemble <run-id>}"
    DAEMON_UP || { step F8 assemble could-not-judge "daemon unreachable — verdict table unassemblable"; exit 0; }
    run_notes "$RUN" | RUN="$RUN" "$PY" -c '
import json, os, sys
from collections import defaultdict
env = json.load(sys.stdin)
run = os.environ["RUN"]
steps = defaultdict(list)
for n in env.get("notes", []):
    for line in (n.get("content") or "").splitlines():
        if not line.startswith("DRILL_STEP"):
            continue
        parts = line.split(" ", 4)
        if len(parts) < 4:
            continue
        _, case, s, r, *rest = parts
        steps[case].append((s, r, rest[0] if rest else ""))
CASES = ["F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8"]
print("commons-fluency drill %s — verdicts assembled from the notes alone (UC-F8):" % run)
for c in CASES:
    rows = steps.get(c, [])
    if not rows:
        print("%-6s never-ran        no steps on either side" % c)
        continue
    if any(r == "FAIL" for _, r, _ in rows):
        print("%-6s failed          %d FAIL step(s): %s" % (c,
              sum(1 for _, r, _ in rows if r == "FAIL"),
              "; ".join("%s %s" % (s, d) for s, r, d in rows if r == "FAIL")))
        continue
    if any(r != "PASS" for _, r, _ in rows):
        print("%-6s could-not-judge %d step(s) not PASS: %s" % (c,
              sum(1 for _, r, _ in rows if r != "PASS"),
              "; ".join("%s: %s" % (s, d) for s, r, d in rows if r != "PASS")))
        continue
    print("%-6s passed          %s" % (c, "; ".join("%s (%s)" % (s, r) for s, r, _ in rows)))
print()
open_cases = [c for c in CASES if steps.get(c) and any(r != "PASS" for _, r, _ in steps[c])]
print("UC-F8: escalations needed = %d (%s) — zero means the drill ran itself" % (
    len(open_cases), ", ".join(open_cases) or "none"))
print("Verdicts are four, not two (ARCH §18.2): passed / failed /")
print("could-not-judge (evidence recorded) / never-ran (not invoked).")
'
    N="$(run_notes "$RUN" | grep -c '"id"' || true)"
    step F8 assemble PASS "verdict table assembled from the run's notes alone ($N notes read)"
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
                or c.startswith("drill-start:") or c.startswith("drill-neg:")
                or c.startswith("DRILL_STEP")
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
  *) echo "usage: co-mesh-drill.sh open|list-check|close|gone-check|note|ambient-check|reply|seen-check|directive|stats-check|d4-check|cleanup|report|f-start|f-exec|f-assemble|f1-take|f1-release|f1-sight|f2-expired|f3-note|f3-origin|f3-receipt|f3-claim|f3-negative|f3-liveness|f4-note|f4-ambient|f4-reply|f4-seen|f5-check|f7-wire ..." >&2
     exit 2 ;;
esac
