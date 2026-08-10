#!/usr/bin/env bash
# Long-soak gate report (DAEMON_RESILIENCE.md P3.2).
#
# Reads a LIVE install's local artifacts — supervisor.log, crash
# records, daemon.err (+ rotated baks), pidfile — and renders a soak
# verdict for the release gate: has this daemon been boringly healthy
# long enough to ship? Local-first by construction: reads files on this
# machine, never sends anything anywhere (no-telemetry posture,
# DAEMON_RESILIENCE.md §1.3).
#
# Usage: scripts/daemon-soak-report.sh [data_dir]   # default ~/.svrnmesh
# Exit:  0 PASS · 1 FAIL · 2 WARN
#
# Verdict heuristics (tune as the fleet teaches us):
#   FAIL — daemon not running, OR >6 supervisor restarts in 24h
#          (crash-looping), OR a panic crash record newer than 24h.
#   WARN — any restart in 24h, soft-limit RSS warnings in the current
#          log window, or stack-overflow markers anywhere in the logs.
set -uo pipefail

DIR="${1:-$HOME/.svrnmesh}"
LOGS="$DIR/logs"
NOW=$(date +%s)
FAILS=0; WARNS=0
fail() { FAILS=$((FAILS+1)); echo "  ✘ $*"; }
warn() { WARNS=$((WARNS+1)); echo "  ⚠ $*"; }
info() { echo "  · $*"; }

echo "── daemon soak report — $DIR — $(date -Is)"

# ── Liveness + uptime ────────────────────────────────────────────────
PID="$(tr -d '[:space:]' < "$DIR/daemon.pid" 2>/dev/null || true)"
if [[ -n "$PID" ]] && kill -0 "$PID" 2>/dev/null; then
  START=$(stat -c %Y "/proc/$PID" 2>/dev/null || echo "$NOW")
  UP_H=$(( (NOW - START) / 3600 ))
  UP_M=$(( ((NOW - START) % 3600) / 60 ))
  info "daemon running: pid $PID, up ${UP_H}h${UP_M}m"
else
  fail "daemon NOT RUNNING (pidfile: '${PID:-absent}')"
fi

# ── Supervisor restarts (exit-code breakdown) ────────────────────────
SUP="$LOGS/supervisor.log"
if [[ -f "$SUP" ]]; then
  TOTAL=$(grep -c "daemon exited" "$SUP" 2>/dev/null || true)
  CUTOFF="$(date -Is -d '24 hours ago' 2>/dev/null || echo "")"
  if [[ -n "$CUTOFF" ]]; then
    # ISO timestamps in one timezone compare lexically.
    LAST24=$(awk -v c="$CUTOFF" '$1 >= c' "$SUP" | grep -c "daemon exited" || true)
  else
    LAST24="?"
  fi
  info "supervisor: $TOTAL restarts ever; $LAST24 in the last 24h"
  if [[ "$TOTAL" -gt 0 ]]; then
    grep -o "code=[0-9-]*" "$SUP" | sort | uniq -c \
      | awk '{printf "      %s × %s\n", $1, $2}' \
      | sed 's/code=102/code=102 (RSS hard limit)/; s/code=104/code=104 (listener lost)/'
    grep "daemon exited" "$SUP" | tail -2 | sed 's/^/      /'
  fi
  if [[ "$LAST24" != "?" && "$LAST24" -gt 6 ]]; then
    fail "crash-looping: $LAST24 restarts in 24h (>6)"
  elif [[ "$LAST24" != "?" && "$LAST24" -gt 0 ]]; then
    warn "$LAST24 restart(s) in the last 24h — check exit codes above"
  fi
else
  warn "no supervisor.log — daemon may be running UNSUPERVISED (crash = down until noticed)"
fi

# ── Crash records (panic hook, P0.4) ─────────────────────────────────
CRASHES="$DIR/crashes"
if [[ -d "$CRASHES" ]]; then
  N=$(find "$CRASHES" -name "daemon-panic-*.json" 2>/dev/null | wc -l)
  info "crash records: $N"
  LAST="$CRASHES/last-crash.json"
  if [[ -f "$LAST" ]]; then
    AGE_H=$(( (NOW - $(stat -c %Y "$LAST")) / 3600 ))
    MSG=$(python3 -c "import json;d=json.load(open('$LAST'));print(f\"{d.get('kind','?')} in {d.get('thread','?')} at {d.get('location','?')}: {d.get('message','?')[:100]}\")" 2>/dev/null || echo "unreadable")
    if [[ "$AGE_H" -lt 24 ]]; then
      fail "crash record ${AGE_H}h old: $MSG"
    else
      info "last crash ${AGE_H}h ago: $MSG"
    fi
  fi
else
  info "crash records: none (dir absent — no Rust panic since deploy)"
fi

# ── daemon.err markers (current window + rotated baks) ───────────────
ERR="$LOGS/daemon.err"
if [[ -f "$ERR" ]]; then
  ARMED=$(grep -a "memory-watch: armed" "$ERR" | tail -1 | sed 's/\x1b\[[0-9;]*m//g')
  [[ -n "$ARMED" ]] && info "OOM defense: ${ARMED#*INFO }"
  grep -aq "hard limit DISABLED" "$ERR" && warn "memory-watch reports HARD LIMIT DISABLED"
  SOFT=$(grep -ac "rss above soft limit" "$ERR" 2>/dev/null || true)
  [[ "$SOFT" -gt 0 ]] && warn "$SOFT soft-limit RSS warning(s) in current log window"
  LW=$(grep -ac "listener-watch: client port not accepting" "$ERR" 2>/dev/null || true)
  [[ "$LW" -gt 0 ]] && warn "$LW listener-watch failed-probe event(s) in current log window"
  DEGRADED=$(grep -ac "DEGRADED until daemon restart" "$ERR" 2>/dev/null || true)
  [[ "$DEGRADED" -gt 0 ]] && fail "$DEGRADED background task(s) parked DEGRADED (restart ceiling hit)"
  OVERFLOW=$(cat "$ERR" "$ERR".bak.* 2>/dev/null | grep -ac "has overflowed its stack" || true)
  [[ "$OVERFLOW" -gt 0 ]] && warn "$OVERFLOW stack-overflow marker(s) across log window + baks (P3.3)"
else
  info "no daemon.err at $LOGS (journal-managed install?)"
fi

# ── Verdict ──────────────────────────────────────────────────────────
echo
if [[ $FAILS -gt 0 ]]; then
  echo "── VERDICT: FAIL ($FAILS failing, $WARNS warning) — not soak-clean"
  exit 1
elif [[ $WARNS -gt 0 ]]; then
  echo "── VERDICT: WARN ($WARNS warning) — review before shipping"
  exit 2
else
  echo "── VERDICT: PASS — boringly healthy"
  exit 0
fi
