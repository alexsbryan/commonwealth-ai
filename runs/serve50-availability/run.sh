#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# RUN — order `serve50-availability`, the §9.1.1 fleet arm at N=2.
#
# ONE ARM PER INVOCATION. Which arm is decided by BeefyMac's state, which
# this script does NOT control and MUST NOT try to:
#
#   ARM "peer-idle"  — nobody at the Mac's keyboard for the duration.
#                      Expect served_by=peer > 0 with attribution.
#   ARM "peer-busy"  — a person actively using the Mac throughout.
#                      Expect ZERO peer dispatches and no failed-hop tax.
#
# Pass the arm name as $1 so the output file says which world it measured.
# Mislabelling the arm is the only way this run can produce a wrong number,
# so it is required rather than inferred.
#
#   runs/serve50-availability/run.sh peer-idle
#   runs/serve50-availability/run.sh peer-busy
#
# ── What this script will NOT do ─────────────────────────────────────────────
# It never restarts, reconfigures, or sends a request at a peer. Peer daemons
# are other machines' constraint (order `mesh-serve-50-red`, Seams). Load only
# ever enters at the LOCAL node's client surface, which is what makes the mesh
# scheduler — the thing under test — do the routing.
#
# ── Precondition it CANNOT check for you ─────────────────────────────────────
# The local daemon must be running a binary built from this branch. A daemon
# restart is the seat's action, not this script's. The census below records the
# fleet it actually saw, but nothing here can tell you whether the daemon's
# code matches the working tree — confirm that before trusting a green.
set -uo pipefail

ARM="${1:-}"
case "$ARM" in
  peer-idle|peer-busy) ;;
  *) echo "usage: $0 <peer-idle|peer-busy>" >&2; exit 2 ;;
esac

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
OUT="$ROOT/runs/serve50-availability"
STAMP="$(date +%Y%m%d_%H%M%S)"
LOG="$OUT/${ARM}_${STAMP}.log"
DECISIONS="$OUT/${ARM}_${STAMP}.decisions.jsonl"

# Exit markers — the caller reads these, not the transcript.
: > "$OUT/RUNNING"
rm -f "$OUT/DONE" "$OUT/FAILED"
finish() {
  local rc=$1
  rm -f "$OUT/RUNNING"
  if [[ $rc -eq 0 ]]; then echo "$LOG" > "$OUT/DONE"; else echo "rc=$rc $LOG" > "$OUT/FAILED"; fi
  exit "$rc"
}

{
  echo "=== serve50-availability fleet arm: $ARM ==="
  echo "started: $(date -Is)"
  echo "branch:  $(git -C "$ROOT" rev-parse --abbrev-ref HEAD)"
  echo "head:    $(git -C "$ROOT" rev-parse --short HEAD)"
  echo

  # Step 1 — census. Never sends a turn. Refuses to score below 2 serving
  # nodes by construction, which is the guard that stops a one-node fleet
  # being reported as a sweep.
  echo "--- census (no load) ---"
  "$ROOT/scripts/probe-serve50-fleet.sh" || { echo "census failed"; finish 1; }
  echo

  # Step 2 — the arm itself. The decision log is the instrument: every
  # exclusion, every refusal, every served_by lands here as one JSON object
  # per line, and it is what the two table rows are read from.
  echo "--- load at LOCAL client surface, 50 clients ---"
  echo "decision log: $DECISIONS"
  SOVEREIGN_DECISION_LOG="$DECISIONS" \
    "$ROOT/scripts/probe-serve50-fleet.sh" \
      --drive --clients 50 \
      --load scripts/probe_serve50_ttft.py \
      --load-args "--reps 2 --max-tokens 64" \
    || { echo "drive failed"; finish 1; }
  echo

  # Step 3 — read the instrument. These four numbers ARE the table row.
  echo "--- decision-log summary ---"
  if [[ -s "$DECISIONS" ]]; then
    python3 - "$DECISIONS" <<'PY'
import json, sys, collections
served = collections.Counter()
excluded = collections.Counter()
failovers = collections.Counter()
yield_refusals = 0
decisions = outcomes = 0
for line in open(sys.argv[1]):
    line = line.strip()
    if not line:
        continue
    try:
        r = json.loads(line)
    except json.JSONDecodeError:
        continue
    ev = r.get("event")
    if ev == "decision":
        decisions += 1
        for x in r.get("excluded", []) or []:
            reason = x.get("reason")
            excluded[reason if isinstance(reason, str) else json.dumps(reason)] += 1
    elif ev == "outcome":
        outcomes += 1
        sb = r.get("served_by") or {}
        served[sb.get("kind", "?")] += 1
        for f in r.get("failovers", []) or []:
            failovers[f.get("peer", "?")] += 1
            if f.get("yield_retry_after_secs") is not None:
                yield_refusals += 1
print(f"decisions: {decisions}  outcomes: {outcomes}")
print(f"served_by: {dict(served)}")
print(f"excluded:  {dict(excluded)}")
print(f"failover hops by peer: {dict(failovers)}")
print(f"yield refusals received: {yield_refusals}")
print()
print("READ THIS AS THE TABLE ROW:")
print(f"  served_by=peer            -> {served.get('peer', 0)}")
print(f"  excluded yielded_to_local -> {excluded.get('yielded_to_local', 0)}")
print(f"  failed hops (yield)       -> {yield_refusals}")
PY
  else
    echo "NO DECISION RECORDS — the log is empty."
    echo "That is a could-not-judge, not a zero: check the daemon's tracing"
    echo "filter carries the mesh.decision target and that it is the binary"
    echo "built from this branch."
  fi

  echo
  echo "finished: $(date -Is)"
} 2>&1 | tee "$LOG"

finish "${PIPESTATUS[0]}"
