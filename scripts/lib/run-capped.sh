#!/usr/bin/env bash
# run-capped.sh — one wall-clock cap for shell drivers, portable to hosts with
# no GNU `timeout`.
#
# WHY THIS IS A LIB
#
# macOS ships neither `timeout` nor `gtimeout` unless coreutils is installed,
# and ten scripts in this directory reach for `timeout` directly. One of them
# is `scripts/tests/release-provenance-gate.sh`, whose whole point is telling a
# HANG apart from a refusal — so on a Mac that suite exited 127 ("command not
# found") and the check it exists for never ran (found 2026-09-03).
#
# The implementation is `sovereign-ci-bench.sh`'s `run_capped`, moved here
# verbatim so there is one decider rather than a second copy that drifts
# (ARCH §10.6). Returns 124 on timeout, matching `timeout(1)`, which is what
# every caller's status switch already reads.

TIMEOUT_BIN="$(command -v timeout 2>/dev/null || command -v gtimeout 2>/dev/null || true)"

# Run "$@" with a hard wall-clock cap in seconds ($1). Returns 124 on timeout,
# matching `timeout(1)`, which is what the lane-status switch already reads.
run_capped() {
  local cap="$1"; shift
  if [[ -n "$TIMEOUT_BIN" ]]; then
    "$TIMEOUT_BIN" "${cap}s" "$@"
    return $?
  fi
  # Shell watchdog. Kills the lane AND its descendants — a bench lane spawns
  # `eval run` children, and TERMing only the parent leaves the model call
  # holding the daemon slot the next lane needs.
  "$@" &
  local pid=$!
  local waited=0
  while kill -0 "$pid" 2>/dev/null; do
    if (( waited >= cap )); then
      pkill -TERM -P "$pid" 2>/dev/null
      kill -TERM "$pid" 2>/dev/null
      local grace=0
      while kill -0 "$pid" 2>/dev/null && (( grace < 10 )); do
        sleep 1; grace=$(( grace + 1 ))
      done
      pkill -KILL -P "$pid" 2>/dev/null
      kill -KILL "$pid" 2>/dev/null
      wait "$pid" 2>/dev/null
      return 124
    fi
    sleep 1
    waited=$(( waited + 1 ))
  done
  wait "$pid"
  return $?
}
