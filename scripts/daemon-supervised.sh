#!/usr/bin/env bash
# In-toolbox daemon supervisor — service-manager semantics where systemd
# can't reach (the daemon must run INSIDE the vulkan toolbox for GPU access;
# a host systemd user unit would relaunch it GPU-broken).
#
# Arms the memory-watch HARD limit (memory_watch.rs): on RSS breach the
# daemon self-SIGTERMs with a NON-ZERO exit — a graceful drain instead of
# the kernel OOM killer's SIGKILL (two dirty deaths on 2026-07-10/11 at
# ~39.5GB) — and this loop relaunches it clean. Soft limit tuned to this
# box's real envelope so warnings mean something.
#
# Usage:  scripts/daemon-supervised.sh            # foreground loop (setsid it)
# Stop:   touch ~/.sovereign/supervised.stop      # loop exits after daemon stops
#         (or `sovereign daemon stop` twice — the loop respects the sentinel)
set -u
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DAEMON="$REPO_ROOT/target/debug/sovereign-cli-daemon"
LOG_DIR="$HOME/.sovereign/logs"
STOP_SENTINEL="$HOME/.sovereign/supervised.stop"
mkdir -p "$LOG_DIR"
rm -f "$STOP_SENTINEL"

export SOVEREIGN_RSS_SOFT_LIMIT_MB="${SOVEREIGN_RSS_SOFT_LIMIT_MB:-28000}"
export SOVEREIGN_RSS_HARD_LIMIT_MB="${SOVEREIGN_RSS_HARD_LIMIT_MB:-36000}"

echo "$(date -Is) supervisor: armed soft=${SOVEREIGN_RSS_SOFT_LIMIT_MB}MB hard=${SOVEREIGN_RSS_HARD_LIMIT_MB}MB" >> "$LOG_DIR/supervisor.log"
while :; do
  "$DAEMON" daemon run >> "$LOG_DIR/daemon.err" 2>&1
  code=$?
  echo "$(date -Is) supervisor: daemon exited code=$code" >> "$LOG_DIR/supervisor.log"
  if [ -f "$STOP_SENTINEL" ]; then
    echo "$(date -Is) supervisor: stop sentinel present — exiting" >> "$LOG_DIR/supervisor.log"
    break
  fi
  if [ "$code" -eq 0 ]; then
    echo "$(date -Is) supervisor: clean exit — not restarting" >> "$LOG_DIR/supervisor.log"
    break
  fi
  sleep 8
done
