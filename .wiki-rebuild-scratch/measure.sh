#!/usr/bin/env bash
# Peak RSS + wall for one build, from the kernel's own high-water mark
# (/proc/<pid>/status VmHWM). Same semantic as `/usr/bin/time -v`'s
# "Maximum resident set size", which is not available inside this toolbox.
set -uo pipefail
LABEL="$1"; shift
t0=$(date +%s.%N)
"$@" > "$SC_OUT/$LABEL.out" 2> "$SC_OUT/$LABEL.err" &
pid=$!
peak=0
while kill -0 "$pid" 2>/dev/null; do
  # the toolbox wrapper's child tree — take the max VmHWM across it
  for p in $(pgrep -P "$pid" 2>/dev/null) "$pid"; do
    v=$(awk '/^VmHWM:/{print $2}' "/proc/$p/status" 2>/dev/null)
    [[ -n "${v:-}" && "$v" -gt "$peak" ]] && peak=$v
  done
  # and the real worker, wherever podman put it
  for p in $(pgrep -f 'sovereign-cli(-llm)? atlas wikipedia' 2>/dev/null); do
    v=$(awk '/^VmHWM:/{print $2}' "/proc/$p/status" 2>/dev/null)
    [[ -n "${v:-}" && "$v" -gt "$peak" ]] && peak=$v
  done
  sleep 0.2
done
wait "$pid"; rc=$?
t1=$(date +%s.%N)
printf 'MEASURE %s rc=%d wall_s=%.1f peak_rss_mb=%.0f\n' \
  "$LABEL" "$rc" "$(echo "$t1 - $t0" | bc)" "$(echo "$peak / 1024" | bc)"
