#!/usr/bin/env bash
# In-toolbox daemon supervisor — service-manager semantics where systemd
# can't reach (the daemon must run INSIDE the vulkan toolbox for GPU access;
# a host systemd user unit would relaunch it GPU-broken).
#
# Relaunches the daemon on the memory-watch exit-102 (RSS hard limit)
# and listener-watch exit-104 (lost client listener) paths — plus any
# crash. Since DAEMON_RESILIENCE.md P0.3 the RSS limits are DERIVED
# IN-DAEMON from total RAM (Linux 85%/70%) and default ON — this script
# no longer hardcodes them (the old 36 GB ceiling on a 128 GB box also
# blocked legitimate 122B loads). Set SOVEREIGN_RSS_{SOFT,HARD}_LIMIT_MB
# in the environment to override; they pass through untouched.
#
# Usage:  scripts/daemon-supervised.sh            # foreground loop (setsid it)
# Stop:   touch ~/.svrnmesh/supervised.stop      # loop exits after daemon stops
#         (or `sovereign daemon stop` twice — the loop respects the sentinel)
set -u
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DAEMON="$REPO_ROOT/target/debug/sovereign-cli-daemon"
LOG_DIR="$HOME/.svrnmesh/logs"
STOP_SENTINEL="$HOME/.svrnmesh/supervised.stop"
mkdir -p "$LOG_DIR"
rm -f "$STOP_SENTINEL"

echo "$(date -Is) supervisor: armed (RSS limits: in-daemon RAM-derived defaults${SOVEREIGN_RSS_HARD_LIMIT_MB:+, env hard=${SOVEREIGN_RSS_HARD_LIMIT_MB}MB}${SOVEREIGN_RSS_SOFT_LIMIT_MB:+, env soft=${SOVEREIGN_RSS_SOFT_LIMIT_MB}MB})" >> "$LOG_DIR/supervisor.log"
while :; do
  "$DAEMON" daemon run >> "$LOG_DIR/daemon.err" 2>&1
  code=$?
  echo "$(date -Is) supervisor: daemon exited code=$code" >> "$LOG_DIR/supervisor.log"
  if [ -f "$STOP_SENTINEL" ]; then
    echo "$(date -Is) supervisor: stop sentinel present — exiting" >> "$LOG_DIR/supervisor.log"
    break
  fi
  # SENTINEL-ONLY exit (2026-07-11 incident): treating exit-0 as "clean,
  # don't restart" collided with operational `daemon stop` — one manual
  # restart ended supervision silently, and every daemon after it ran
  # unsupervised AND without the RSS env, straight into a kernel OOM kill.
  # Deliberate shutdown is: touch ~/.svrnmesh/supervised.stop && daemon stop.
  echo "$(date -Is) supervisor: daemon exited (code=$code) — restarting" >> "$LOG_DIR/supervisor.log"
  sleep 8
done
