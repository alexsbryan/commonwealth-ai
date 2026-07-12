#!/usr/bin/env bash
# Segmented persona soak. The daemon's memory envelope is owned by the
# memory-watch hard limit + supervisor (scripts/daemon-supervised.sh) — this
# script does NOT manage the daemon; it just waits for health and collects
# per-segment journals so a supervised restart mid-soak costs one wait, not
# the night.
#
# HISTORY (2026-07-11): v1 pointed ART at a nonexistent dir; every segment's
# node launch failed on the log redirect and the loop degenerated into 4,478
# empty daemon-restart cycles. Hence: mkdir -p, a first-segment sanity gate,
# and a consecutive-empty-segment abort. Silence is not success.
#
# Usage: soak-persona.sh <total_minutes> [segment_minutes] [stamp-prefix]
set -u
TOTAL_MIN=${1:-240}
SEG_MIN=${2:-45}
PREFIX=${3:-soak}
E2E_DIR="$(cd "$(dirname "$0")/.." && pwd)"
CRATE_ROOT="$(cd "$E2E_DIR/../.." && pwd)"
ART="$CRATE_ROOT/test-artifacts"
mkdir -p "$ART"
END=$(( $(date +%s) + TOTAL_MIN * 60 ))
i=0
empty_streak=0
rm -f "$ART/$PREFIX.DONE"
while [ $(( $(date +%s) + SEG_MIN * 60 )) -le "$END" ]; do
  # A segment starts only if it FITS the remaining budget — checking at
  # loop-top let a segment starting at minute 99 of 100 run 45 min past
  # END (observed 2026-07-11: silent 15-min overrun before the operator
  # asked). Same boundary bug as the driver's per-turn cap, one level up.
  i=$((i + 1))
  # Wait (up to 5 min) for a healthy daemon — a supervised restart may be
  # in flight. No health after 5 min = abort loudly.
  ok=""
  for _ in $(seq 1 30); do
    if curl -s -m 5 http://127.0.0.1:9741/healthz >/dev/null 2>&1; then ok=1; break; fi
    sleep 10
  done
  if [ -z "$ok" ]; then
    echo "segment $i: daemon unreachable for 5 min — aborting soak" | tee -a "$ART/$PREFIX-runner.log"
    break
  fi
  echo "── segment $i starting ──"
  ( cd "$CRATE_ROOT" && node tests/e2e/scripts/personas.mjs --attach --spawn \
      --corpora sep,wikipedia,wikipedia-simple,federalist-starter \
      --sessions 0 --minutes "$SEG_MIN" --max-searches 8 \
      > "$ART/$PREFIX-seg$i.log" 2>&1 )
  cp "$ART/persona-journal.jsonl" "$ART/$PREFIX-seg$i.jsonl" 2>/dev/null
  turns=$(grep -c '"kind":"turn"' "$ART/$PREFIX-seg$i.jsonl" 2>/dev/null || echo 0)
  echo "── segment $i done: $turns turns ──"
  if [ "$turns" -eq 0 ]; then
    empty_streak=$((empty_streak + 1))
    if [ "$empty_streak" -ge 2 ]; then
      echo "two consecutive empty segments — aborting soak (see $PREFIX-seg$i.log)" | tee -a "$ART/$PREFIX-runner.log"
      break
    fi
  else
    empty_streak=0
  fi
done
date > "$ART/$PREFIX.DONE"
echo "soak complete: $i segments"
