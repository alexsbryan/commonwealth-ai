#!/usr/bin/env bash
# Daemon resilience soak (DAEMON_RESILIENCE.md P3.1 seed).
#
# Boots a REAL daemon in an isolated HOME on non-default ports with the
# small soak models, then exercises the failure modes the P0 hardening
# is supposed to make boring:
#
#   attach — the external-daemon topology (CLI/launchd/systemd + desktop
#            Attach mode): an HTTP client polls readiness while the
#            daemon is kill -9'd and relaunched by a supervisor loop
#            (the service-manager stand-in). Asserts recovery within
#            budget on every cycle. This is exactly what the desktop's
#            attach watcher (attach_watch.rs) observes.
#   child  — the SAME cycling, but the daemon is the desktop binary
#            re-entering itself as `--daemon-child` (the W1-flip
#            production path for users who never touch a CLI). Skipped
#            with a notice when the desktop binary isn't built.
#   guards — process-identity checks: second `daemon run` refused by the
#            flock run lock; SIGTERM stop exits 0 and releases pidfile;
#            optional SOAK_RSS=1 drill asserts the RSS hard limit exits
#            102 (the supervised-relaunch contract).
#
# Isolation: fresh HOME under mktemp, client/internal ports 19741/19742,
# mDNS off, iroh kill-switched — the soak daemon can never gossip with a
# live mesh. Requires models/bonsai-8b.gguf + models/qwen-embedding-0.6b.gguf
# checkouts (the two smallest local GGUFs).
#
# Usage: scripts/daemon-soak.sh [--cycles N] [--case attach|child|guards|all]
#   SOAK_RSS=1        also run the RSS exit-102 drill (adds ~2 min)
#   SOAK_PORT=19741   override the client port
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DAEMON_BIN="${DAEMON_BIN:-$REPO_ROOT/target/debug/sovereign-cli-daemon}"
CHILD_BIN="${CHILD_BIN:-$REPO_ROOT/target/debug/sovereign-desktop}"
PRIMARY_GGUF="$REPO_ROOT/models/bonsai-8b.gguf/Bonsai-8B-Q1_0.gguf"
EMBED_GGUF="$REPO_ROOT/models/qwen-embedding-0.6b.gguf/Qwen3-Embedding-0.6B-Q8_0.gguf"

CYCLES=5
CASE="all"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --cycles) CYCLES="$2"; shift 2 ;;
    --case) CASE="$2"; shift 2 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

CLIENT_PORT="${SOAK_PORT:-19741}"
INTERNAL_PORT=$((CLIENT_PORT + 1))
READY_BUDGET_SECS="${READY_BUDGET_SECS:-120}"

SOAK_HOME="$(mktemp -d /tmp/daemon-soak.XXXXXX)"
LOG_DIR="$SOAK_HOME/.sovereign/logs"
mkdir -p "$SOAK_HOME/.sovereign" "$LOG_DIR"

# Soak env: isolated HOME; iroh kill-switch; lock/pid all under SOAK_HOME.
soak_env() {
  env HOME="$SOAK_HOME" SOVEREIGN_IROH=off RUST_BACKTRACE=1 "$@"
}

cat > "$SOAK_HOME/.sovereign/config.toml" <<EOF
[models]
primary = "$PRIMARY_GGUF"
embed = "$EMBED_GGUF"
context_size = 4096

[daemon]
client_port = $CLIENT_PORT
internal_port = $INTERNAL_PORT

[data]
dir = "$SOAK_HOME/.sovereign"

[discovery]
mdns = false
EOF

PASS=0
FAIL=0
declare -a FAILURES=()
note() { echo "[soak] $*"; }
ok()   { PASS=$((PASS+1)); echo "[soak]   ✓ $*"; }
bad()  { FAIL=$((FAIL+1)); FAILURES+=("$*"); echo "[soak]   ✘ $*"; }

DAEMON_PID=""
declare -a SPAWNED_PIDS=()
cleanup() {
  # Kill every pid we ever spawned (tracked, never pkill-by-name — the
  # operator's live daemon may run the same binary path).
  local p
  for p in "${SPAWNED_PIDS[@]:-}"; do
    [[ -n "$p" ]] && kill -9 "$p" 2>/dev/null
  done
  if [[ ${FAIL:-0} -eq 0 ]]; then
    rm -rf "$SOAK_HOME"
  else
    echo "[soak] failures — keeping $SOAK_HOME for triage (logs under .sovereign/logs)"
  fi
}
trap cleanup EXIT

probe() { curl -sf -m 2 "http://127.0.0.1:$CLIENT_PORT/v1/models" >/dev/null 2>&1; }

wait_ready() { # $1 = budget secs; returns 0 + echoes elapsed
  local start elapsed
  start=$(date +%s)
  while :; do
    if probe; then
      elapsed=$(( $(date +%s) - start ))
      echo "$elapsed"
      return 0
    fi
    elapsed=$(( $(date +%s) - start ))
    if (( elapsed >= $1 )); then
      echo "$elapsed"
      return 1
    fi
    sleep 1
  done
}

start_daemon() { # $1 = binary, remaining = args; sets DAEMON_PID
  local bin="$1"; shift
  # Direct `env … cmd &` so $! IS the daemon (env execs in place).
  # Backgrounding a shell FUNCTION here made $! the subshell's pid —
  # kill -9 then killed the subshell and ORPHANED the daemon, which
  # kept serving. Found on the first smoke run (2026-07-18); same
  # class as the mesh chaos runner's harness bash bug.
  env HOME="$SOAK_HOME" SOVEREIGN_IROH=off RUST_BACKTRACE=1 "$bin" "$@" \
    >>"$LOG_DIR/soak-daemon.out" 2>>"$LOG_DIR/soak-daemon.err" &
  DAEMON_PID=$!
  SPAWNED_PIDS+=("$DAEMON_PID")
}

# The daemon's bootstrap writes its OWN pid to the pidfile; asserting it
# matches what we spawned proves the answering listener is OUR daemon —
# an orphan answering the probe can't fake this. POLLED, not one-shot:
# the pidfile write is the LAST bootstrap step (daemon_cmd/mod.rs
# write_pidfile), seconds after the port binds, so a ready-probe can
# legitimately beat it — a one-shot check here produced 2 flaky
# failures in the first full soak (2026-07-18).
pidfile_matches() {
  local j
  for (( j=0; j<40; j++ )); do
    if [[ "$(tr -d '[:space:]' < "$SOAK_HOME/.sovereign/daemon.pid" 2>/dev/null)" == "$DAEMON_PID" ]]; then
      return 0
    fi
    sleep 0.5
  done
  return 1
}

stop_daemon_hard() {
  [[ -n "$DAEMON_PID" ]] && kill -9 "$DAEMON_PID" 2>/dev/null
  wait "$DAEMON_PID" 2>/dev/null
  DAEMON_PID=""
}

# ── Case: kill/relaunch cycling for one binary flavor ────────────────
run_cycles() { # $1 = label, $2 = binary, remaining = args
  local label="$1" bin="$2"; shift 2
  note "case $label: cold boot"
  start_daemon "$bin" "$@"
  local t
  if t=$(wait_ready "$READY_BUDGET_SECS") && pidfile_matches; then
    ok "$label: cold boot ready in ${t}s (pid $DAEMON_PID, pidfile matches)"
  else
    bad "$label: cold boot NOT ready/identified within ${READY_BUDGET_SECS}s — aborting case (see $LOG_DIR/soak-daemon.err)"
    stop_daemon_hard
    return 1
  fi

  local i
  for (( i=1; i<=CYCLES; i++ )); do
    kill -9 "$DAEMON_PID" 2>/dev/null
    wait "$DAEMON_PID" 2>/dev/null
    if kill -0 "$DAEMON_PID" 2>/dev/null; then
      bad "$label cycle $i: process survived kill -9 (harness pid tracking broken)"
    fi
    # Down must be observable (the attach watcher's raise condition).
    if probe; then
      bad "$label cycle $i: port still answering after kill -9 (orphan listener?)"
    fi
    # Supervisor stand-in: relaunch (flock is kernel-released on SIGKILL,
    # so this also asserts no stale-lock wedge).
    start_daemon "$bin" "$@"
    if t=$(wait_ready "$READY_BUDGET_SECS") && pidfile_matches; then
      ok "$label cycle $i: recovered in ${t}s after kill -9 (pid $DAEMON_PID owns the pidfile)"
    else
      bad "$label cycle $i: NOT recovered/identified within ${READY_BUDGET_SECS}s"
      stop_daemon_hard
      return 1
    fi
  done
  stop_daemon_hard
  # Let the port close fully before the next case boots.
  sleep 2
  return 0
}

# ── Case: guards ─────────────────────────────────────────────────────
run_guards() {
  note "case guards: single-instance + stop semantics"
  start_daemon "$DAEMON_BIN" daemon run
  local t
  if ! t=$(wait_ready "$READY_BUDGET_SECS"); then
    bad "guards: boot not ready within ${READY_BUDGET_SECS}s"
    stop_daemon_hard
    return 1
  fi

  # Second `daemon run` under the same HOME must be refused fast by the
  # flock run lock, without loading models.
  local second_rc second_err
  second_err="$SOAK_HOME/second-run.err"
  soak_env timeout 30 "$DAEMON_BIN" daemon run >/dev/null 2>"$second_err"
  second_rc=$?
  if [[ $second_rc -ne 0 && $second_rc -ne 124 ]] && grep -qi "run lock" "$second_err"; then
    ok "guards: second daemon run refused (rc=$second_rc, run-lock message present)"
  else
    bad "guards: second daemon run NOT refused (rc=$second_rc; stderr: $(head -c 200 "$second_err"))"
  fi

  # First daemon must be UNHARMED by the refused second run.
  if probe; then
    ok "guards: original daemon still serving after refused second run"
  else
    bad "guards: original daemon stopped serving after refused second run"
  fi

  # SIGTERM stop: deliberate shutdown = exit 0, pidfile removed.
  kill -TERM "$DAEMON_PID"
  local stop_rc
  wait "$DAEMON_PID"; stop_rc=$?
  DAEMON_PID=""
  if [[ $stop_rc -eq 0 ]]; then
    ok "guards: SIGTERM stop exited 0 (deliberate-shutdown contract)"
  else
    bad "guards: SIGTERM stop exited $stop_rc (expected 0)"
  fi
  if [[ ! -f "$SOAK_HOME/.sovereign/daemon.pid" ]]; then
    ok "guards: pidfile removed on stop"
  else
    bad "guards: pidfile still present after stop"
  fi

  # Optional: RSS hard-limit drill — tiny limit ⇒ graceful exit 102
  # within ~2 sampler ticks (sampler ticks at 60s).
  if [[ "${SOAK_RSS:-0}" == "1" ]]; then
    note "guards: RSS exit-102 drill (takes ~1-2 min)"
    env HOME="$SOAK_HOME" SOVEREIGN_IROH=off RUST_BACKTRACE=1 \
      SOVEREIGN_RSS_HARD_LIMIT_MB=64 "$DAEMON_BIN" daemon run \
      >>"$LOG_DIR/soak-daemon.out" 2>>"$LOG_DIR/soak-daemon.err" &
    DAEMON_PID=$!
    SPAWNED_PIDS+=("$DAEMON_PID")
    local rss_rc
    # Poll for exit (a subshell can't `wait` on this shell's child),
    # then reap in THIS shell to read the exit code.
    for (( j=0; j<200; j++ )); do
      kill -0 "$DAEMON_PID" 2>/dev/null || break
      sleep 1
    done
    if kill -0 "$DAEMON_PID" 2>/dev/null; then
      bad "guards: RSS drill — daemon did not exit within 200s of a 64MB hard limit"
      stop_daemon_hard
    else
      wait "$DAEMON_PID"; rss_rc=$?
      DAEMON_PID=""
      if [[ $rss_rc -eq 102 ]]; then
        ok "guards: RSS hard limit exited 102 (supervised-relaunch contract)"
      else
        bad "guards: RSS drill exited $rss_rc (expected 102)"
      fi
    fi
  fi
  return 0
}

# ── Preflight ────────────────────────────────────────────────────────
[[ -x "$DAEMON_BIN" ]] || { echo "missing $DAEMON_BIN — build sovereign-cli-daemon (debug)"; exit 2; }
[[ -f "$PRIMARY_GGUF" ]] || { echo "missing $PRIMARY_GGUF"; exit 2; }
[[ -f "$EMBED_GGUF" ]] || { echo "missing $EMBED_GGUF"; exit 2; }
if probe; then
  echo "something already answers on :$CLIENT_PORT — pick another SOAK_PORT" >&2
  exit 2
fi
note "HOME=$SOAK_HOME ports=$CLIENT_PORT/$INTERNAL_PORT cycles=$CYCLES case=$CASE"

# ── Run ──────────────────────────────────────────────────────────────
if [[ "$CASE" == "attach" || "$CASE" == "all" ]]; then
  run_cycles "attach" "$DAEMON_BIN" daemon run
fi
if [[ "$CASE" == "child" || "$CASE" == "all" ]]; then
  if [[ -x "$CHILD_BIN" ]]; then
    run_cycles "child(--daemon-child)" "$CHILD_BIN" --daemon-child
  else
    note "case child: SKIPPED — $CHILD_BIN not built (cargo build -p sovereign-desktop)"
  fi
fi
if [[ "$CASE" == "guards" || "$CASE" == "all" ]]; then
  run_guards
fi

# ── Summary ──────────────────────────────────────────────────────────
echo
echo "[soak] ────────────────────────────────"
echo "[soak] pass: $PASS  fail: $FAIL"
for f in "${FAILURES[@]:-}"; do [[ -n "$f" ]] && echo "[soak]   FAILED: $f"; done
echo "[soak] daemon logs: $LOG_DIR (removed on exit — rerun with 'trap - EXIT' edit to keep)"
[[ $FAIL -eq 0 ]]
