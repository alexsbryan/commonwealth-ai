#!/bin/bash
# Capture tracing rows emitted by a shadow/counterfactual arm, before log
# rotation eats them.
#
# WHY THIS EXISTS — the near-miss it was extracted from (order audit-economy,
# 2026-08-14, directive f15729b2). The claim-search ladder's flip bar was
# `lost_rescue == 0` over the arm's shadow rows. The capture step grepped ONLY
# ~/.svrnmesh/logs/daemon.err — but the gate runs in the DESKTOP process under
# attach mode, so the rows are in desktop.out.log. The daemon-only grep wrote an
# EMPTY file, and `lost_rescue == 0` passes trivially on a file that recorded
# nothing. A default flip would have shipped on a bar that could not fail.
# Caught by the seat mid-arm; the flip was then re-judged on 160 real rows.
#
# THE STRUCTURAL PART (ARCH §18.1, §18.3): an empty capture is a FAILURE, not a
# pass. This script exits non-zero and names every source it searched rather
# than leaving a zero-byte file for a downstream gate to read as "no losses".
# If you write a new arm that greps logs for its verdict rows, use this — or
# reproduce the loud-empty check yourself. Do not grep straight into a file the
# gate will read.
#
# Usage: capture_shadow_rows.sh <pattern> <out-file> <source-log> [source-log...]
#   e.g. capture_shadow_rows.sh claim_search_shadow "$D/shadow_rows.log" \
#          "$D/desktop.out.log" ~/.svrnmesh/logs/daemon.err
set -uo pipefail

if [ "$#" -lt 3 ]; then
  echo "capture: usage: $0 <pattern> <out-file> <source-log> [source-log...]" >&2
  exit 2
fi

PATTERN="$1"; shift
OUT="$1"; shift

present=()
missing=()
for src in "$@"; do
  if [ -r "$src" ]; then present+=("$src"); else missing+=("$src"); fi
done

if [ "${#missing[@]}" -gt 0 ]; then
  echo "capture: WARNING unreadable source(s): ${missing[*]}" >&2
fi

if [ "${#present[@]}" -eq 0 ]; then
  echo "capture: FAILED — no readable source logs among: $*" >&2
  exit 1
fi

# -a: these logs can carry binary chunks; without it grep silently reports
# "binary file matches" and writes nothing.
grep -ah "$PATTERN" "${present[@]}" > "$OUT" 2>/dev/null

rows=$(wc -l < "$OUT" | tr -d ' ')
if [ "${rows:-0}" -eq 0 ]; then
  echo "capture: FAILED — 0 rows matching '$PATTERN' in: ${present[*]}" >&2
  echo "capture: an empty capture is could-not-judge, NOT a passing gate." >&2
  echo "capture: check which PROCESS emits the rows (desktop vs daemon, attach" >&2
  echo "capture: mode puts the gate in the desktop) and whether rotation ate" >&2
  echo "capture: them — the window is ~6h." >&2
  exit 1
fi

echo "capture: $rows row(s) matching '$PATTERN' -> $OUT (sources: ${present[*]})"
