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

# ── The decision log is the DAEMON's, and we ASK it where that is ────────────
#
# 2026-08-14, the defect this replaces: this script used to EXPORT
# SOVEREIGN_DECISION_LOG pointing at a per-run file it chose. That variable is
# read by the daemon (decision_log.rs:68, consumed peer_inference.rs:693) — and
# the daemon is a long-lived process started by the service manager, which
# never saw our export. It kept writing to the path baked into its own
# ExecStart line. The script then read its own empty file and reported
# could-not-judge while 624 real records for that very window sat in the
# daemon's log. The verdict was honest and the instrument was pointed at
# nothing, which is the §18.4 failure exactly: validate the instrument before
# the result.
#
# The fix is to stop DICTATING and start ASKING. The daemon's own environment
# is the single source of truth for where it writes; anything else is a second
# decider for one path (§10.6).
DECISIONS=""
DECISIONS_SOURCE=""
daemon_pid="$(pgrep -f 'sovereign-cli-daemon' | head -1 || true)"
if [[ -n "$daemon_pid" && -r "/proc/$daemon_pid/environ" ]]; then
  DECISIONS="$(tr '\0' '\n' < "/proc/$daemon_pid/environ" \
    | sed -n 's/^S\(OVEREIGN\|VRNMESH\)_DECISION_LOG=//p' | head -1 || true)"
  [[ -n "$DECISIONS" ]] && DECISIONS_SOURCE="daemon pid $daemon_pid environment"
fi

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
  # The daemon must already be writing decision records. We cannot arm a
  # running daemon from here — the variable is read at construction — so an
  # unarmed daemon is a REFUSAL before any load is sent, never a run whose
  # verdict turns out to be unreadable after the peer's window is spent.
  echo "--- decision-log instrument ---"
  if [[ -z "$DECISIONS" ]]; then
    echo "COULD-NOT-JUDGE: the running daemon has no decision-log path in its"
    echo "environment, so this arm would produce load with no attribution."
    echo "Refusing BEFORE the run rather than after (the peer's idle window is"
    echo "the scarce resource here)."
    echo
    echo "Arm it on the ExecStart line of the systemd drop-in"
    echo "  ~/.config/systemd/user/sovereign.service.d/40-toolbox.conf"
    echo "as 'env SOVEREIGN_DECISION_LOG=<path>' — an Environment= line does"
    echo "NOT survive 'toolbox run' (that drop-in's own header says so) — then"
    echo "restart via 'sovereign daemon stop && sovereign daemon start'."
    finish 3
  fi
  echo "path:   $DECISIONS"
  echo "source: $DECISIONS_SOURCE"
  if [[ ! -r "$DECISIONS" ]]; then
    echo "COULD-NOT-JUDGE: the daemon names $DECISIONS but it is not readable here."
    finish 3
  fi
  # The window is what makes a CUMULATIVE log readable as one arm. The daemon
  # appends forever, so an unscoped count mixes today's arm with every prior
  # run — how a first read of this file showed 53,227 records for a 3-minute
  # measurement.
  WIN_START_MS=$(( $(date +%s) * 1000 ))
  echo "window opens: $WIN_START_MS"
  echo

  echo "--- load at LOCAL client surface, 50 clients ---"
  "$ROOT/scripts/probe-serve50-fleet.sh" \
      --drive --clients 50 \
      --load scripts/probe_serve50_ttft.py \
      --load-args "--reps 2 --max-tokens 64" \
    || { echo "drive failed"; finish 1; }
  WIN_END_MS=$(( ($(date +%s) + 5) * 1000 ))
  echo

  # Step 3 — read the instrument. These four numbers ARE the table row.
  echo "--- decision-log summary (scoped to this arm's window) ---"
  python3 - "$DECISIONS" "$WIN_START_MS" "$WIN_END_MS" "$ARM" <<'PY'
import json, sys, collections
path, lo, hi, arm = sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), sys.argv[4]
served = collections.Counter()
excluded = collections.Counter()
failovers = collections.Counter()
yield_refusals = 0
decisions = outcomes = in_window = total = undated = 0
for line in open(path):
    line = line.strip()
    if not line:
        continue
    try:
        r = json.loads(line)
    except json.JSONDecodeError:
        continue
    total += 1
    ts = r.get("ts_unix_ms")
    if ts is None:
        undated += 1
        continue
    # The log is CUMULATIVE and append-only: every prior run is in this file.
    # Scoping to the window is what makes it this arm's number rather than the
    # machine's lifetime total.
    if not (lo <= ts <= hi):
        continue
    in_window += 1
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
print(f"log records total: {total}   in this window: {in_window}   undated: {undated}")
print(f"decisions: {decisions}  outcomes: {outcomes}")
print(f"served_by: {dict(served)}")
print(f"excluded:  {dict(excluded)}")
print(f"failover hops by peer: {dict(failovers)}")
print(f"yield refusals received: {yield_refusals}")
print()
if in_window == 0:
    # Records exist but none are ours: the daemon is writing somewhere we are
    # not reading, or it was restarted mid-arm. Either way it is not a zero.
    print("COULD-NOT-JUDGE: zero records fall inside this arm's window.")
    print("The load ran; the attribution did not reach this file. Do NOT read")
    print("the load numbers above as a table row.")
else:
    print(f"READ THIS AS THE TABLE ROW (arm: {arm}):")
    print(f"  served_by=peer            -> {served.get('peer', 0)}")
    print(f"  served_by=local           -> {served.get('local', 0)}")
    print(f"  served_by=local_fallback  -> {served.get('local_fallback', 0)}")
    print(f"  excluded yielded_to_local -> {excluded.get('yielded_to_local', 0)}")
    print(f"  excluded quarantined      -> {excluded.get('quarantined', 0)}")
    print(f"  failed hops (yield)       -> {yield_refusals}")
PY

  echo
  echo "finished: $(date -Is)"
} 2>&1 | tee "$LOG"

finish "${PIPESTATUS[0]}"
