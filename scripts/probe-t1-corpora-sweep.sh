#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# RED BASELINE SWEEP — order `mesh-scale-t1-red` (seat amendment 0ab79301),
# bars `t1-expansion-scoped` + `t1-prefilter-per-turn`.
#
# ONE QUESTION: what SHAPE does per-turn retrieval wall trace as the installed
# corpus count grows? A point measurement at n=1000 says the system is slow
# there; only a curve says whether the Tier-1 fixes have to flatten a line or
# bend a knee. Five log-spaced points, N turns each.
#
# The headline number is the SLOPE — seconds of per-turn retrieval wall per
# 100 corpora — not the n=1000 point. The report names the shape (linear /
# superlinear / flat) and stops there: five points is a sense of the curve,
# not a model.
#
# HOW: one master rig of index clones (built exactly as Probe B builds it),
# then a SYMLINK FARM per sweep point selecting the first n clones. Symlinks,
# not copies: the search path only reads, and re-cloning 94 MB per point buys
# nothing but wall time.
#
# Usage:
#   scripts/probe-t1-corpora-sweep.sh --master <dir-of-1000-clones> \
#     [--points "10 50 100 316 1000"] [--turns 3] [--prefilter K] [--set K=V]
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MASTER=""
POINTS="10 50 100 316 1000"
TURNS=3
PREFILTER=""
QUESTION="What did the Finch Array record near the Lighthouse at Sable Point, and who reviewed it?"
declare -a EXTRA=()

while [[ $# -gt 0 ]]; do
  case "$1" in
    --master)    MASTER="$2"; shift 2 ;;
    --points)    POINTS="$2"; shift 2 ;;
    --turns)     TURNS="$2"; shift 2 ;;
    --prefilter) PREFILTER="$2"; shift 2 ;;
    --question)  QUESTION="$2"; shift 2 ;;
    --set)       EXTRA+=(--set "$2"); shift 2 ;;
    -h|--help)   sed -n '3,25p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *) echo "unknown flag: $1" >&2; exit 2 ;;
  esac
done
[[ -d "$MASTER" ]] || { echo "sweep: --master must be a directory of index clones" >&2; exit 2; }

FARM="${TMPDIR:-/tmp}/probe-t1-sweep-$$"
mkdir -p "$FARM"
trap 'rm -rf "$FARM"' EXIT

mapfile -t ALL < <(find "$MASTER" -maxdepth 1 -mindepth 1 -type d | sort)
echo "sweep: master has ${#ALL[@]} clones; points: $POINTS; turns per point: $TURNS"
echo "sweep: prefilter=${PREFILTER:-off} question=\"$QUESTION\""

for n in $POINTS; do
  (( n <= ${#ALL[@]} )) || { echo "sweep: skipping n=$n (master has only ${#ALL[@]})"; continue; }
  DIR="$FARM/n$n"
  mkdir -p "$DIR"
  for ((i = 0; i < n; i++)); do
    ln -s "${ALL[$i]}" "$DIR/$(basename "${ALL[$i]}")"
  done
  echo
  echo "════ sweep point n=$n ════════════════════════════════════════════"
  PF=()
  [[ -n "$PREFILTER" ]] && PF=(--prefilter "$PREFILTER")
  "$ROOT/scripts/probe-t1-expansion-fanout.sh" --rig "$DIR" --turns "$TURNS" \
    --question "$QUESTION" "${PF[@]}" "${EXTRA[@]}" \
    | grep -E "^PROBE_T1|^probe-t1: (BIND CHECK PASSED|warm-up exit|turn [0-9]+ exit)|COULD-NOT-JUDGE"
done

echo
echo "sweep: done. Fit the slope from the per-point retrieval_ms brackets and"
echo "       record the table + the shape in MESH_SCALE_100_USERS_1000_CORPORA.md §8.3."
