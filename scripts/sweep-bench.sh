#!/usr/bin/env bash
#
# Sweep the agent-coding bench across (model, config) cells, then run
# `sovereign-agent-bench aggregate` over the resulting artifact tree
# to print a failure-class histogram.
#
# v0 scope: one model (commonwealth/coder), two configs (force=0 and
# force=1), one or more problems (passed via $PROBLEMS, comma-sep,
# default "1.1,3.2"). Each cell is a separate daemon restart so the
# `SOVEREIGN_FORCE_TOOL_CALLS` env can be toggled.
#
# Output layout:
#   /tmp/agent-bench-sweep-<utc-date>/
#     coder-force0/
#       1.1-reverse-string/
#         agent.json
#         witness.json
#         ...
#       3.2-lights-out/
#         ...
#     coder-force1/
#       ...
#
# Usage:
#   scripts/sweep-bench.sh                       # defaults
#   PROBLEMS=3.2 scripts/sweep-bench.sh          # one problem
#   AGENT_MODEL=commonwealth/fast \              # override
#   JUDGE_MODEL=commonwealth/primary \
#   PROBLEMS=1.1,3.2 \
#   scripts/sweep-bench.sh
#
# Implementation notes:
# - `sovereign daemon stop` + `sovereign daemon start` cleanly per cell.
# - `SOVEREIGN_DISABLE_AUTO_RESUME=1` always set; we don't want corpus
#   resume jobs eating fast-slot RSS during the bench.
# - Wall-cap per cell is the per-problem cap × N problems plus a slop;
#   the harness already enforces a per-problem cap so we don't time
#   the outer loop.
# - Exit is non-zero if any individual `sovereign-agent-bench run`
#   crashes (so CI / wrapping callers see the failure). Per-cell scores
#   are NOT cell-pass-fail; that's what aggregate is for.

set -euo pipefail

REPO_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." &> /dev/null && pwd)"
cd "$REPO_ROOT"

BENCH_BIN="${BENCH_BIN:-./target/release/sovereign-agent-bench}"
PROBLEMS="${PROBLEMS:-1.1,3.2}"
AGENT_MODEL="${AGENT_MODEL:-commonwealth/coder}"
JUDGE_MODEL="${JUDGE_MODEL:-commonwealth/primary}"
JUDGE_TRIALS="${JUDGE_TRIALS:-1}"
DAEMON_BIN="${DAEMON_BIN:-sovereign}"

UTC_DATE="$(date -u +%Y-%m-%d-%H%M%S)"
SWEEP_ROOT="${SWEEP_ROOT:-/tmp/agent-bench-sweep-${UTC_DATE}}"
mkdir -p "$SWEEP_ROOT"

echo "sweep-bench: root=$SWEEP_ROOT"
echo "sweep-bench: agent=$AGENT_MODEL judge=$JUDGE_MODEL problems=$PROBLEMS"
echo

# Sanity: bench binary present.
if [[ ! -x "$BENCH_BIN" ]]; then
  echo "sweep-bench: $BENCH_BIN not found or not executable" >&2
  exit 2
fi

run_cell() {
  local cell_name="$1"
  local force_flag="$2"      # "0" or "1"

  echo "==== cell $cell_name (force_tool_calls=$force_flag) ===="
  echo "sweep-bench: restarting daemon ..."
  "$DAEMON_BIN" daemon stop || true
  # Daemon picks up env vars from its own process environment.
  SOVEREIGN_FORCE_TOOL_CALLS="$force_flag" \
    SOVEREIGN_DISABLE_AUTO_RESUME=1 \
    "$DAEMON_BIN" daemon start

  # Wait briefly for the daemon listener to come up. `daemon start`
  # already blocks until ready, but on cold launches the slot
  # registry takes another second or two. Cheap belt-and-braces.
  sleep 2

  local cell_dir="$SWEEP_ROOT/$cell_name"
  local report_path="$cell_dir/report.json"
  mkdir -p "$cell_dir"

  set +e
  "$BENCH_BIN" run \
    --problems "$PROBLEMS" \
    --model "$AGENT_MODEL" \
    --judge-model "$JUDGE_MODEL" \
    --judge-trials "$JUDGE_TRIALS" \
    --report "$report_path" \
    --artifacts-dir "$cell_dir"
  local rc=$?
  set -e
  echo "sweep-bench: cell $cell_name exit=$rc report=$report_path"
  echo
  return $rc
}

# Two cells: force=0 (no grammar lock) and force=1 (grammar lock).
# Extend by adding more invocations of `run_cell <name> <force>`.
CELL_FAILED=0
run_cell "coder-force0" "0" || CELL_FAILED=1
run_cell "coder-force1" "1" || CELL_FAILED=1

echo "==== aggregate ===="
"$BENCH_BIN" aggregate --root "$SWEEP_ROOT" --list-paths \
  --json-out "$SWEEP_ROOT/aggregate.json"

if (( CELL_FAILED )); then
  echo
  echo "sweep-bench: at least one cell exited non-zero (see logs above)" >&2
  exit 1
fi
