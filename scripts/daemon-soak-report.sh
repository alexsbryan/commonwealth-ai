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
#        scripts/daemon-soak-report.sh --self-test   # watch the FAIL fire
# Exit:  0 PASS · 1 FAIL · 2 WARN
#
# Verdict heuristics (tune as the fleet teaches us):
#   FAIL — daemon not running, OR >6 supervisor restarts in 24h
#          (crash-looping), OR a panic crash record newer than 24h.
#   WARN — any restart in 24h, soft-limit RSS warnings in the current
#          log window, or stack-overflow markers anywhere in the logs.
set -uo pipefail

SELF_TEST=0
[[ "${1:-}" == "--self-test" ]] && { SELF_TEST=1; shift; }

DIR="${1:-$HOME/.svrnmesh}"
LOGS="$DIR/logs"
NOW=$(date +%s)
# ONE 24-hour cutoff, computed once, used by every rate below. GNU form
# first, BSD second — see the portability note under `mtime`.
CUTOFF="$(date -u -d '24 hours ago' +%Y-%m-%dT%H:%M:%S 2>/dev/null \
          || date -u -v-24H +%Y-%m-%dT%H:%M:%S 2>/dev/null || echo "")"
STAMP="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
FAILS=0; WARNS=0
# ── Portability, and why it is not cosmetic ──────────────────────────
# Until 2026-09-08 this script read `stat -c %Y` and `date -Is -d` — both
# GNU-only. On darwin the first SILENTLY substituted "now" for the process
# start time and reported every daemon as `up 0h0m`; the second left the
# 24-hour cutoff empty, so `LAST24` became "?" and the crash-loop FAIL — the
# script's headline verdict — could not be reached at all. A gate with no
# input that can fail it is not a gate (ARCH §18.1), and a substitution that
# does not announce itself is §18.3. One accessor each, both platforms.
mtime() { stat -f %m "$1" 2>/dev/null || stat -c %Y "$1" 2>/dev/null; }
# Elapsed seconds for a live pid. `ps -o etime=` is POSIX and needs no /proc.
pid_uptime_secs() {
  local e; e="$(ps -p "$1" -o etime= 2>/dev/null | tr -d ' ')"
  [[ -z "$e" ]] && { echo ""; return; }
  local d=0 rest="$e"
  case "$rest" in *-*) d="${rest%%-*}"; rest="${rest#*-}";; esac
  local IFS=:; set -- $rest
  case $# in
    3) echo $(( d*86400 + $1*3600 + $2*60 + $3 )) ;;
    2) echo $(( d*86400 + $1*60 + $2 )) ;;
    *) echo "" ;;
  esac
}

fail() { FAILS=$((FAILS+1)); echo "  ✘ $*"; }
warn() { WARNS=$((WARNS+1)); echo "  ⚠ $*"; }
info() { echo "  · $*"; }

# ── --self-test: the negative control ────────────────────────────────
#
# A report nobody has watched go RED is a report nobody should believe
# (ARCH §18.1). This builds a PAIR that differs in ONE thing — the age of the
# daemon's exit receipts — and requires the verdict to flip. Recent exits must
# FAIL; the same eight exits three days old must not. Both arms run the real
# script against a synthetic data dir, so what is exercised is the shipped
# path and not a copy of it.
if [[ "$SELF_TEST" == "1" ]]; then
  SELF_RC=0
  fixture() { # dir, iso-stamp-prefix
    local d="$1" when="$2" i
    mkdir -p "$d/logs"
    echo $$ > "$d/daemon.pid"
    : > "$d/logs/daemon.err"
    for i in 1 2 3 4 5 6 7 8; do
      printf '%s:0%s.000000Z  WARN daemon: shutdown signal received\n' \
        "$when" "$i" >> "$d/logs/daemon.err"
    done
  }
  T="$(mktemp -d)"
  trap 'rm -rf "$T"' EXIT

  fixture "$T/hot" "$(date -u -d '1 hour ago' +%Y-%m-%dT%H:%M 2>/dev/null \
                      || date -u -v-1H +%Y-%m-%dT%H:%M)"
  fixture "$T/cold" "$(date -u -d '3 days ago' +%Y-%m-%dT%H:%M 2>/dev/null \
                       || date -u -v-3d +%Y-%m-%dT%H:%M)"

  "$0" "$T/hot" >/dev/null 2>&1; HOT=$?
  "$0" "$T/cold" >/dev/null 2>&1; COLD=$?

  if [[ "$HOT" -eq 1 ]]; then
    echo "  ✔ MUTANT CAUGHT — 8 daemon exits inside 24h renders FAIL (exit 1)"
  else
    echo "  ✘ MUTANT MISSED — 8 daemon exits inside 24h rendered exit $HOT, not 1"
    SELF_RC=1
  fi
  if [[ "$COLD" -ne 1 ]]; then
    echo "  ✔ CONTROL CLEAN — the same 8 exits, 3 days old, does not FAIL (exit $COLD)"
  else
    echo "  ✘ CONTROL DIRTY — 3-day-old exits also rendered FAIL; the window is not read"
    SELF_RC=1
  fi
  echo
  if [[ "$SELF_RC" -eq 0 ]]; then
    echo "── SELF-TEST: PASS — the crash-loop verdict has a failing input and a clean one"
  else
    echo "── SELF-TEST: FAIL — this report cannot be trusted to notice a restart loop"
  fi
  exit "$SELF_RC"
fi

echo "── daemon soak report — $DIR — $STAMP"

# ── Liveness + uptime ────────────────────────────────────────────────
PID="$(tr -d '[:space:]' < "$DIR/daemon.pid" 2>/dev/null || true)"
if [[ -n "$PID" ]] && kill -0 "$PID" 2>/dev/null; then
  UP_S="$(pid_uptime_secs "$PID")"
  if [[ -n "$UP_S" ]]; then
    info "daemon running: pid $PID, up $(( UP_S / 3600 ))h$(( (UP_S % 3600) / 60 ))m"
  else
    # Named, never defaulted: an unknown uptime is not a zero one.
    info "daemon running: pid $PID, uptime UNAVAILABLE (ps gave no etime)"
  fi
else
  fail "daemon NOT RUNNING (pidfile: '${PID:-absent}')"
fi

# ── Supervisor restarts (exit-code breakdown) ────────────────────────
SUP="$LOGS/supervisor.log"
if [[ -f "$SUP" ]]; then
  TOTAL=$(grep -c "daemon exited" "$SUP" 2>/dev/null || true)
  if [[ -n "$CUTOFF" ]]; then
    # ISO timestamps in one timezone compare lexically. The supervisor
    # stamps LOCAL time and the cutoff is UTC, so west of Greenwich this
    # window is wide by the offset — it over-counts, never under-counts,
    # which is the safe direction for a health verdict.
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

# ── Exits the daemon logged for ITSELF (works with no supervisor) ────
#
# WHY THIS EXISTS. Everything above keys on `supervisor.log`, which only
# `scripts/daemon-supervised.sh` writes. On a launchd or systemd install —
# i.e. every real deployment and this dev box — that file does not exist, so
# the block above emits one WARN and the crash-loop FAIL is unreachable. On
# 2026-09-08 the daemon went down THREE times in nine hours on the host this
# report was written for, and a run of this script would have said
# "boringly healthy". A check no input can fail is not a check (ARCH §18.1).
#
# The daemon's own shutdown receipt is the supervisor-independent signal:
# `lifecycle.rs::log_shutdown_context` writes one line per exit with a
# spelling its own doc comment pins as stable, and the rotation is read too.
if [[ -f "$LOGS/daemon.err" ]]; then
  RECEIPTS=$(cat "$LOGS/daemon.err" "$LOGS"/daemon.err.*.bak 2>/dev/null \
             | grep -ac "daemon: shutdown signal received" || true)
  if [[ -n "$CUTOFF" ]]; then
    EXITS24=$(cat "$LOGS/daemon.err" "$LOGS"/daemon.err.*.bak 2>/dev/null \
              | grep -a "daemon: shutdown signal received" \
              | sed 's/\x1b\[[0-9;]*m//g' \
              | grep -oE '^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}' \
              | awk -v c="$CUTOFF" '$0 >= c' | wc -l | tr -d ' ')
    info "daemon exits (own receipts): $RECEIPTS ever; $EXITS24 in the last 24h"
    if [[ "$EXITS24" -gt 6 ]]; then
      fail "daemon exited $EXITS24 time(s) in 24h (>6) — restart-looping"
    elif [[ "$EXITS24" -gt 0 ]]; then
      warn "daemon exited $EXITS24 time(s) in the last 24h"
    fi
  else
    # The cutoff is the only thing that makes the count a rate. Without it
    # there is no verdict to give, and saying so beats giving a wrong one.
    info "daemon exits (own receipts): $RECEIPTS ever; 24h window UNAVAILABLE (no portable date)"
  fi
  # A receipt above 24 GiB is the daemon GUESSING at jetsam from its own
  # peak RSS (`SIGTERM && rss >= 24 GiB`). It is a lead, not a diagnosis:
  # on a box whose daemon idles above that line every operator stop matches.
  # `scripts/daemon-concurrency-soak.py` is what corroborates one against the
  # kernel; this report only counts them.
  JETSAM=$(cat "$LOGS/daemon.err" "$LOGS"/daemon.err.*.bak 2>/dev/null \
           | grep -ac "peak RSS suggests possible jetsam" || true)
  [[ "$JETSAM" -gt 0 ]] && warn "$JETSAM exit(s) carried the daemon's own jetsam SUSPICION (unconfirmed — see daemon-concurrency-soak)"
fi

# ── Crash records (panic hook, P0.4) ─────────────────────────────────
CRASHES="$DIR/crashes"
if [[ -d "$CRASHES" ]]; then
  N=$(find "$CRASHES" -name "daemon-panic-*.json" 2>/dev/null | wc -l)
  info "crash records: $N"
  LAST="$CRASHES/last-crash.json"
  if [[ -f "$LAST" ]]; then
    AGE_H=$(( (NOW - $(mtime "$LAST")) / 3600 ))
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
  # The rotation writes `daemon.err.<epoch>.bak`, so the old `"$ERR".bak.*`
  # glob matched NOTHING and this scan silently read only the live window.
  OVERFLOW=$(cat "$ERR" "$ERR".*.bak 2>/dev/null | grep -ac "has overflowed its stack" || true)
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
