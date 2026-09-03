#!/usr/bin/env bash
# pod-supervise.sh — keep a long-lived process alive, and say so out loud.
#
# Written for the rented-GPU daemon (`scripts/dev-pod.sh`), which has to
# survive a multi-hour bench run. It is a separate file, not a heredoc inside
# the pod's onstart script, for one reason: the first version WAS a heredoc,
# it had a bug that only appears when the supervised process leaves a child
# behind, and nothing could reach it to test. `scripts/tests/pod-supervise.sh`
# is the gate.
#
# WHY A DAEMON NEEDS SUPERVISING AT ALL. A failed `GGML_ASSERT` inside
# llama_decode calls `abort()`, so an over-long prompt kills the PROCESS
# rather than failing the request. Under `exec` that is terminal: nothing is
# left to restart it, and a four-hour run reports "daemon unreachable" from
# the crash onward.
#
# THE BUG THIS FILE EXISTS TO NOT HAVE. The obvious loop is
#
#     while :; do  "$@" 2>&1 | tee -a "$LOG";  done
#
# and it hangs. bash waits for EVERY member of a pipeline, and `tee` only
# sees EOF when the LAST holder of the write end closes it. A daemon that
# spawns children (compute child, workers) leaves them holding that pipe when
# it aborts — so `tee` never returns, the loop never comes round, and the
# supervisor sits there looking alive while nothing is serving. Observed
# 2026-09-03 on pod 49783403: `bash /.launch` present, no daemon process, no
# restart banner, port dead.
#
# So: run the command in the BACKGROUND, remember ITS pid, and `wait` on that
# pid alone. Orphaned children can hold the log fd as long as they like; they
# cannot hold the supervisor. Log visibility is kept by mirroring the file to
# stdout with `tail -F` (the container log is what `dev-pod.sh logs` reads, so
# writing only to a file would make the daemon invisible from outside).
#
# Usage:  pod-supervise.sh <logfile> <command> [args...]
#
# Env:
#   SUPERVISE_MAX_STARTS   stop after N starts (default 0 = forever). The test
#                          uses it; a pod leaves it unset.
#   SUPERVISE_YOUNG_SECS   a start that dies sooner than this is "young"
#                          (default 60).
#   SUPERVISE_YOUNG_BACKOFF  seconds to wait after a young death (default 60),
#                          so a broken loadout does not spin a rented GPU.
#   SUPERVISE_BACKOFF      seconds to wait after a normal death (default 5).
set -uo pipefail

LOG="${1:?usage: pod-supervise.sh <logfile> <command> [args...]}"
shift
[ "$#" -gt 0 ] || { echo "pod-supervise: no command given" >&2; exit 2; }

MAX_STARTS="${SUPERVISE_MAX_STARTS:-0}"
YOUNG_SECS="${SUPERVISE_YOUNG_SECS:-60}"
YOUNG_BACKOFF="${SUPERVISE_YOUNG_BACKOFF:-60}"
BACKOFF="${SUPERVISE_BACKOFF:-5}"

mkdir -p "$(dirname "$LOG")" 2>/dev/null || true
: >> "$LOG"

# Mirror the log to our stdout so the container log carries it. Started once,
# follows the file across restarts, and is cleaned up on exit.
tail -n0 -F "$LOG" 2>/dev/null &
MIRROR_PID=$!
cleanup() { kill "$MIRROR_PID" 2>/dev/null; }
trap cleanup EXIT INT TERM

starts=0
while :; do
  starts=$((starts + 1))
  began=$(date +%s)
  echo "[supervise] start #$starts at $(date -u +%FT%TZ): $*"

  # THE POINT OF THE WHOLE FILE: background the command, wait on ITS pid.
  # No pipeline, so no dependence on a child closing the log fd.
  "$@" >> "$LOG" 2>&1 &
  child=$!
  wait "$child"
  code=$?

  ran=$(( $(date +%s) - began ))
  echo "[supervise] EXITED — start #$starts, status $code, alive ${ran}s"

  if [ "$MAX_STARTS" -gt 0 ] && [ "$starts" -ge "$MAX_STARTS" ]; then
    echo "[supervise] reached SUPERVISE_MAX_STARTS=$MAX_STARTS — stopping"
    exit "$code"
  fi

  if [ "$ran" -lt "$YOUNG_SECS" ]; then
    echo "[supervise] died in under ${YOUNG_SECS}s — backing off ${YOUNG_BACKOFF}s (crash loop?)"
    sleep "$YOUNG_BACKOFF"
  else
    sleep "$BACKOFF"
  fi
done
