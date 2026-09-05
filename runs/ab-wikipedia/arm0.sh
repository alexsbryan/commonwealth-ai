#!/usr/bin/env bash
# ei-7c A/B arm 0 — "today": the INSTALLED wikipedia store, untouched.
# Two runs, so the lane's own run-to-run spread is measured before any arm is
# compared against it (ARCH §18.5: one run is not a measurement).
set -uo pipefail
WT=/home/alexbryan/dev/ei7c-wt
OUT=$WT/runs/ab-wikipedia
mkdir -p "$OUT"
cd "$WT"
for i in 1 2; do
  echo "=== arm0 run $i ===" >&2
  /usr/bin/env - HOME="$HOME" PATH="$PATH" \
    ./target/debug/sovereign-cli eval run \
      --bank sovereign/bench/wikipedia/questions.toml \
      --limit 10 --format json --output "$OUT/arm0-run$i.json" \
      > "$OUT/arm0-run$i.txt" 2> "$OUT/arm0-run$i.err"
  echo "$?" > "$OUT/arm0-run$i.rc"
  awk '/^VmHWM/{print $2}' /proc/self/status 2>/dev/null >/dev/null
done
date -Is > "$OUT/ARM0_DONE"
