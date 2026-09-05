#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Peak RSS + wall for one command, from the kernel's own high-water mark
# (/proc/<pid>/status VmHWM). Same semantic as `/usr/bin/time -v`'s "Maximum
# resident set size", which is NOT available inside this toolbox (no GNU time).
#
# Shape reused from `.wiki-rebuild-scratch/measure.sh` (ARCH §19) — the only
# change is that the watched process tree is the caller's, not a hard-coded
# `atlas wikipedia` pattern.
#
# The dispatcher `sovereign` EXECs into `sovereign-cli-llm`, which keeps the
# PID and gives the new image a fresh mm — so VmHWM read on that PID is the
# SIBLING's high-water, which is the number step 0 is about.
set -uo pipefail
LABEL="$1"; shift
t0=$(date +%s%N)
"$@" > "/tmp/measure-$LABEL.out" 2> "/tmp/measure-$LABEL.err" &
pid=$!
peak=0
while kill -0 "$pid" 2>/dev/null; do
  for p in "$pid" $(pgrep -P "$pid" 2>/dev/null); do
    v=$(awk '/^VmHWM:/{print $2}' "/proc/$p/status" 2>/dev/null)
    [[ -n "${v:-}" && "$v" -gt "$peak" ]] && peak=$v
  done
  sleep 0.02
done
wait "$pid"; rc=$?
t1=$(date +%s%N)
# `bc` is NOT installed in the sovereign-vulkan toolbox (measured 2026-09-05),
# so the arithmetic is bash integer math over nanoseconds. A helper that dies
# on a missing tool while still printing a plausible `wall_s=0.00` is the
# silent-substitution failure this workspace refuses (ARCH §18.3).
printf 'MEASURE %s rc=%d wall_s=%d.%02d peak_rss_kb=%d peak_rss_mb=%d\n' \
  "$LABEL" "$rc" "$(( (t1-t0)/1000000000 ))" "$(( ((t1-t0)/10000000)%100 ))" \
  "$peak" "$(( peak/1024 ))"
