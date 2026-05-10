#!/usr/bin/env bash
# Phase 3 — live lifecycle verification for `sovereign init` /
# `sovereign serve --background` / `sovereign stop` / daemon-takeover.
#
# Run this in a separate terminal when your daemon is stopped.
# It does NOT touch your home `.sovereign/` outside of the
# server.pid pointer (which is exactly what we're testing).
#
# Verifies the five steps in the Phase 3 plan:
#   1. fresh project + serve --background → :9741/mcp/stats == 200
#   2. (covered by unit test serve_skip_when_daemon_running)
#   3. daemon takes over standalone serve PID
#   4. stop kills the background serve and removes the pid file
#   5. SIGKILL'd serve leaves a stale pid; re-running --background overwrites
#
# Step 3 (daemon takeover) is exercised via the embedded daemon test
# in sovereign-mesh::daemon::takeover_tests; running the live
# variant requires `sovereign daemon run` to be available, which
# means a real config + model. That step is only attempted if
# SOVEREIGN_VERIFY_DAEMON_TAKEOVER=1 is set.
#
# Usage:
#   sovereign daemon stop  # if your daemon is running
#   ./scripts/phase3-lifecycle-verify.sh
#
# Or with daemon takeover (slower; needs setup config):
#   SOVEREIGN_VERIFY_DAEMON_TAKEOVER=1 ./scripts/phase3-lifecycle-verify.sh

set -euo pipefail

BIN="${SOVEREIGN_BIN:-$(pwd)/target/release/sovereign-cli}"
if [[ ! -x "$BIN" ]]; then
  BIN="$(pwd)/target/debug/sovereign-cli"
fi
if [[ ! -x "$BIN" ]]; then
  echo "✗ no sovereign-cli binary found. Build first: cargo build -p sovereign-cli"
  exit 2
fi

# Ensure :9741 is free before we start. The whole point of the test
# is that init/serve owns the port; if anything else holds it the
# spawn check below will fail loudly.
if curl -s -m 1 -o /dev/null http://127.0.0.1:9741/mcp/stats; then
  echo "✗ something is already listening on :9741. Stop the daemon first."
  echo "  $BIN daemon stop    # or kill the process"
  exit 2
fi

WORK=$(mktemp -d -t sov-phase3-XXXXXX)
HOME_PID="$HOME/.sovereign/server.pid"
PROJ_PID="$WORK/.sovereign/server.pid"
trap 'cleanup' EXIT

cleanup() {
  # Best-effort: stop anything we spawned + remove tempdir.
  "$BIN" stop >/dev/null 2>&1 || true
  rm -rf "$WORK"
}

step() { printf "\n▶ %s\n" "$1"; }
ok() { printf "  ✓ %s\n" "$1"; }
fail() { printf "  ✗ %s\n" "$1"; exit 1; }

# ---------------------------------------------------------------
step "1) serve --background spawns from a fresh project dir"
cd "$WORK"
mkdir -p .sovereign
"$BIN" serve --background 2>&1 | tee /tmp/sov-phase3-spawn.log

[[ -f "$PROJ_PID" ]] || fail "project pid file missing at $PROJ_PID"
[[ -f "$HOME_PID" ]] || fail "home pid file missing at $HOME_PID"
PID=$(cat "$PROJ_PID")
ok "spawned pid=$PID, project + home pid pointers written"

# Five 200ms attempts: axum should be up almost immediately.
for _ in 1 2 3 4 5; do
  sleep 0.3
  if curl -s -m 1 -o /dev/null -w "%{http_code}" http://127.0.0.1:9741/mcp/stats | grep -q 200; then
    ok ":9741/mcp/stats returned 200"
    break
  fi
done

# ---------------------------------------------------------------
step "5) SIGKILL the serve and verify --background cleans the stale pid"
kill -KILL "$PID" || true
sleep 0.3
# Process is gone but the pid file lingers — that's the bug case.
[[ -f "$PROJ_PID" ]] || fail "expected stale pid file after SIGKILL"

"$BIN" serve --background 2>&1 | tee /tmp/sov-phase3-respawn.log
NEWPID=$(cat "$PROJ_PID")
[[ "$NEWPID" != "$PID" ]] || fail "respawn reused the dead pid: $PID"
ok "stale pid replaced (was $PID, now $NEWPID)"

curl -s -m 1 -o /dev/null -w "%{http_code}" http://127.0.0.1:9741/mcp/stats | grep -q 200 \
  || fail "respawn :9741 not reachable"
ok "respawn :9741/mcp/stats == 200"

# ---------------------------------------------------------------
step "4) stop kills the background serve"
"$BIN" stop 2>&1 | tee /tmp/sov-phase3-stop.log
[[ ! -f "$PROJ_PID" ]] || fail "stop did not remove project pid file"
sleep 0.3
if curl -s -m 1 -o /dev/null http://127.0.0.1:9741/mcp/stats; then
  fail "stop did not actually shut down the server"
fi
ok ":9741/mcp/stats now refuses (server stopped)"

# ---------------------------------------------------------------
if [[ "${SOVEREIGN_VERIFY_DAEMON_TAKEOVER:-0}" == "1" ]]; then
  step "3) daemon takes over a running standalone serve"
  cd "$WORK"
  "$BIN" serve --background >/dev/null
  PID=$(cat "$PROJ_PID")
  ok "standalone serve up at pid=$PID"

  # Boot the daemon in the background; kill it after the takeover check.
  "$BIN" daemon run >/tmp/sov-phase3-daemon.log 2>&1 &
  DAEMON_PID=$!
  for _ in $(seq 1 30); do
    sleep 1
    # Daemon takeover removes ~/.sovereign/server.pid as part of its
    # bind handshake, so the absence of the file is the proof of
    # takeover. The daemon's own /v1/models endpoint then proves
    # the takeover succeeded (vs. just killing serve).
    if [[ ! -f "$HOME_PID" ]] && \
       curl -s -m 1 -o /dev/null -w "%{http_code}" http://127.0.0.1:9741/v1/models | grep -q 200; then
      ok "daemon took over :9741 and removed the standalone pid pointer"
      kill "$DAEMON_PID" 2>/dev/null || true
      break
    fi
  done
  if kill -0 "$DAEMON_PID" 2>/dev/null; then
    kill "$DAEMON_PID"
    fail "daemon never took over :9741 (see /tmp/sov-phase3-daemon.log)"
  fi
fi

echo
echo "✓ Phase 3 lifecycle verification passed."
