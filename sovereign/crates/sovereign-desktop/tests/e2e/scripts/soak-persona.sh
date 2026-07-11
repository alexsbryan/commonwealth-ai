#!/usr/bin/env bash
# Segmented persona soak — works WITH the ggml high-water envelope instead of
# fighting it: the daemon's memory high-water grows with the largest prompt
# seen (long threads → toward the 32k window → ~40GB → OOM alongside dev
# tooling; two deaths on 2026-07-10/11). A fresh daemon per segment keeps
# every window inside the safe envelope while the soak collects continuous
# large-N data. Each segment stamps its own journal; feed them all to
# persona-scoreboard.mjs / persona-gap-atlas.mjs.
#
# Usage: soak-persona.sh <total_minutes> [segment_minutes] [stamp-prefix]
set -u
TOTAL_MIN=${1:-240}
SEG_MIN=${2:-45}
PREFIX=${3:-soak}
E2E_DIR="$(cd "$(dirname "$0")/.." && pwd)"
REPO_ROOT="$(cd "$E2E_DIR/../../../.." && pwd)"
CLI="$REPO_ROOT/target/debug/sovereign-cli"
ART="$E2E_DIR/test-artifacts"
END=$(( $(date +%s) + TOTAL_MIN * 60 ))
i=0
rm -f "$ART/$PREFIX.DONE"
while [ "$(date +%s)" -lt "$END" ]; do
  i=$((i + 1))
  echo "── segment $i: recycling daemon (envelope reset) ──"
  timeout 60 "$CLI" daemon stop >/dev/null 2>&1
  sleep 4
  timeout 180 "$CLI" daemon start >/dev/null 2>&1
  if ! curl -s -m 6 http://127.0.0.1:9741/healthz >/dev/null; then
    echo "daemon failed to start at segment $i — aborting soak"
    break
  fi
  ( cd "$E2E_DIR/.." && node tests/e2e/scripts/personas.mjs --attach --spawn \
      --corpora sep,wikipedia,wikipedia-simple,federalist-starter \
      --sessions 0 --minutes "$SEG_MIN" --max-searches 8 \
      > "$ART/$PREFIX-seg$i.log" 2>&1 )
  cp "$ART/persona-journal.jsonl" "$ART/$PREFIX-seg$i.jsonl" 2>/dev/null
  echo "── segment $i done: $(grep -c '"kind":"turn"' "$ART/$PREFIX-seg$i.jsonl" 2>/dev/null) turns ──"
done
date > "$ART/$PREFIX.DONE"
echo "soak complete: $i segments"
