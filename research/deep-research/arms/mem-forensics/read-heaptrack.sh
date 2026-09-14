#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# read-heaptrack.sh — the fixed readout for a heaptrack-daemon.sh run, so the
# numbers come from one recipe rather than whatever flags were typed that day.
#
#   peaks.txt              top stacks by bytes held at the run's peak, + summary
#   leaks.txt              top stacks still allocated at exit
#   peaks-open_index.txt   the same, restricted to stacks through open_index
#   peaks-search.txt       the same, restricted to stacks through search_with_rerank
#   massif.out             per-stack consumption over time (ms_print / massif-visualizer)
#
# Each pass re-parses the whole trace, so this takes minutes and real memory.
# Run INSIDE the toolbox, AFTER the daemon has exited:
#   toolbox run -c sovereign-vulkan research/deep-research/arms/mem-forensics/read-heaptrack.sh <run-dir>
set -euo pipefail
DIR=${1:?usage: read-heaptrack.sh <run-dir>}
T="$DIR/heaptrack.daemon.zst"

[ -f /run/.containerenv ] || { echo "refusing: not inside the toolbox (heaptrack_print lives there)" >&2; exit 2; }
[ -s "$T" ] || { echo "refusing: no trace at $T" >&2; exit 2; }
if pgrep -x sovereign-cli-d >/dev/null; then
  echo "refusing: a daemon is still running — the trace may not be closed, and a multi-GB" >&2
  echo "heaptrack_print beside a loaded daemon is the co-tenant pressure the run measures" >&2
  exit 2
fi

OUT="$DIR/readout"
mkdir -p "$OUT"
heaptrack_print -f "$T" --print-peaks -n 25 -s 8 > "$OUT/peaks.txt"
heaptrack_print -f "$T" --print-peaks=0 --print-leaks -n 25 -s 8 > "$OUT/leaks.txt"
heaptrack_print -f "$T" --print-peaks -n 10 -s 5 --filter-bt-function open_index > "$OUT/peaks-open_index.txt"
heaptrack_print -f "$T" --print-peaks -n 10 -s 5 --filter-bt-function search_with_rerank > "$OUT/peaks-search.txt"
heaptrack_print -f "$T" --print-peaks=0 -M "$OUT/massif.out" --massif-threshold 2 > /dev/null

for f in peaks peaks-open_index peaks-search leaks; do
  echo "== $f"
  grep -E '^(total runtime|calls to allocation|peak heap memory consumption|peak RSS|total memory leaked)' "$OUT/$f.txt" || echo "   (no summary lines — read the file)"
done
