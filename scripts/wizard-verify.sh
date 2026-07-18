#!/usr/bin/env bash
# First-post-wizard-session relaunch verification (DAEMON_RESILIENCE.md
# P0.1(b)) — the journey-level companion to scripts/daemon-soak.sh.
#
# Drives the REAL desktop binary through the fresh-install wizard flow:
# boots it with a fresh HOME (wizard state), invokes `complete_setup`
# through the command bridge's production IPC path (the same code path
# the wizard's JS uses), then asserts the supervised-relaunch chain:
#
#   1. complete_setup triggers the relaunch (log line),
#   2. the old instance exits,
#   3. a NEW desktop instance appears, resolves Local{CliSetup},
#      and spawns `--daemon-child`,
#   4. the child daemon owns :9741 + the pidfile + the flock,
#   5. the new instance logs the supervised Attach switch.
#
# Isolation: self-wraps in `unshare -r -n` (private netns) so
# :9741/:9745 are free and the run can never touch a live daemon or
# mesh. X11/Wayland still work (filesystem sockets), so two brief app
# windows may appear. Requires the DEBUG desktop build (the command
# bridge is #[cfg(debug_assertions)]) and the small soak models.
#
# Provenance: the first run of this drive (2026-07-18) caught a real
# pre-existing bug — `mirror_to_setup_config`'s no-op short-circuit
# meant fresh desktop-only installs NEVER wrote config.toml, so the
# supervised path would never have engaged for pure-desktop users.
# Unit tests were green; the journey was broken. Keep this drive in
# the pre-release chaos lane.
#
# Log-grep gotchas encoded below (do not "simplify" them away):
# desktop log lines carry ANSI codes between field names and values —
# grep human phrases ("source: CliSetup"), never "field=value"; the
# Attach-switch line lands ~2s after :9741 readiness — poll it.
set -uo pipefail

# ── Self-wrap into a private netns ───────────────────────────────────
if [[ "${WIZARD_VERIFY_NS:-}" != "1" ]]; then
  exec unshare -r -n env WIZARD_VERIFY_NS=1 "$0" "$@"
fi
ip link set lo up

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="${DESKTOP_BIN:-$REPO_ROOT/target/debug/sovereign-desktop}"
PRIMARY_GGUF="${PRIMARY_GGUF:-$REPO_ROOT/models/bonsai-8b.gguf/Bonsai-8B-Q1_0.gguf}"
EMBED_GGUF="${EMBED_GGUF:-$REPO_ROOT/models/qwen-embedding-0.6b.gguf/Qwen3-Embedding-0.6B-Q8_0.gguf}"
FRESH="$(mktemp -d /tmp/wizard-verify.XXXXXX)"
LOG="$FRESH/desktop.log"

[[ -x "$BIN" ]] || { echo "missing $BIN — build sovereign-desktop (debug)"; exit 2; }
[[ -f "$PRIMARY_GGUF" ]] || { echo "missing $PRIMARY_GGUF"; exit 2; }
[[ -f "$EMBED_GGUF" ]] || { echo "missing $EMBED_GGUF"; exit 2; }

export HOME="$FRESH"
export SOVEREIGN_COMMAND_BRIDGE=1
export SOVEREIGN_IROH=off
export RUST_BACKTRACE=1
unset SOVEREIGN_FORCE_LOCAL SOVEREIGN_USE_SUPERVISOR SOVEREIGN_CLI_PATH 2>/dev/null || true

PASS=0; FAIL=0
ok()  { PASS=$((PASS+1)); echo "  ✓ $*"; }
bad() { FAIL=$((FAIL+1)); echo "  ✘ $*"; }

declare -a KILL_PIDS=()
cleanup() {
  local p
  for p in "${KILL_PIDS[@]:-}"; do [[ -n "$p" ]] && kill -9 "$p" 2>/dev/null; done
  # Belt-and-braces: kill anything whose env carries OUR fresh HOME
  # (never pkill by name — a live desktop may run the same binary).
  for p in $(pgrep -f "sovereign-desktop" 2>/dev/null); do
    if tr '\0' '\n' < "/proc/$p/environ" 2>/dev/null | grep -q "^HOME=$FRESH$"; then
      kill -9 "$p" 2>/dev/null
    fi
  done
  if [[ ${FAIL:-0} -eq 0 ]]; then
    rm -rf "$FRESH"
  else
    echo "[verify] failures — keeping $FRESH for triage (log: $LOG)"
  fi
}
trap cleanup EXIT

echo "[verify] launching fresh-install desktop (wizard state; HOME=$FRESH)"
"$BIN" > "$LOG" 2>&1 &
APP1=$!
KILL_PIDS+=("$APP1")

for i in $(seq 1 60); do
  curl -sf -m 1 http://127.0.0.1:9745/healthz >/dev/null 2>&1 && break
  kill -0 "$APP1" 2>/dev/null || { echo "app1 died during boot"; tail -40 "$LOG"; exit 1; }
  sleep 1
done
if curl -sf -m 1 http://127.0.0.1:9745/healthz >/dev/null 2>&1; then
  ok "bridge up on :9745 (app booted to wizard state)"
else
  bad "bridge never came up"; tail -40 "$LOG"; exit 1
fi

echo "[verify] invoking complete_setup via the production invoke path"
PAYLOAD=$(printf '{"cmd":"complete_setup","args":{"setup":{"model_path":"%s","primary_model_path":"%s","embed_model_path":"%s","data_dir":"%s"}}}' \
  "$PRIMARY_GGUF" "$PRIMARY_GGUF" "$EMBED_GGUF" "$FRESH/.sovereign")
# The app exits ~600ms into this command (that's the point) — the HTTP
# reply may or may not make it out; either is fine.
curl -s -m 30 -X POST http://127.0.0.1:9745/invoke \
  -H 'content-type: application/json' -d "$PAYLOAD" || true
echo

# ── 1+2: old instance exits after announcing the relaunch ──────────
for i in $(seq 1 30); do kill -0 "$APP1" 2>/dev/null || break; sleep 1; done
if kill -0 "$APP1" 2>/dev/null; then
  bad "app1 still alive 30s after complete_setup (no relaunch?)"
else
  ok "app1 exited after complete_setup"
fi
if grep -aq "relaunching into the supervised topology" "$LOG"; then
  ok "app1 logged the supervised relaunch"
else
  bad "no 'relaunching into the supervised topology' in app1 log"
fi

# ── 3: new instance + --daemon-child appear ────────────────────────
APP2=""; CHILD=""
for i in $(seq 1 90); do
  for p in $(pgrep -f "sovereign-desktop" 2>/dev/null); do
    [[ "$p" == "$APP1" ]] && continue
    if tr '\0' '\n' < "/proc/$p/environ" 2>/dev/null | grep -q "^HOME=$FRESH$"; then
      if tr '\0' ' ' < "/proc/$p/cmdline" | grep -q -- "--daemon-child"; then
        CHILD="$p"
      else
        APP2="$p"
      fi
    fi
  done
  [[ -n "$APP2" && -n "$CHILD" ]] && break
  sleep 1
done
[[ -n "$APP2" ]] && KILL_PIDS+=("$APP2")
[[ -n "$CHILD" ]] && KILL_PIDS+=("$CHILD")
if [[ -n "$APP2" ]]; then ok "relaunched desktop instance running (pid $APP2)"; else bad "no relaunched desktop instance found"; fi
if [[ -n "$CHILD" ]]; then ok "supervised --daemon-child running (pid $CHILD)"; else bad "no --daemon-child process found"; fi

# ── 4: child owns :9741 + pidfile + flock + mirrored config ────────
READY=""
for i in $(seq 1 120); do
  curl -sf -m 2 http://127.0.0.1:9741/v1/models >/dev/null 2>&1 && { READY=1; break; }
  sleep 1
done
if [[ -n "$READY" ]]; then ok ":9741 serving in the netns"; else bad ":9741 never answered"; fi
# write_pidfile is the LAST bootstrap step (daemon_cmd/mod.rs) — poll.
PIDFILE=""
for i in $(seq 1 40); do
  PIDFILE="$(tr -d '[:space:]' < "$FRESH/.sovereign/daemon.pid" 2>/dev/null || true)"
  [[ -n "$CHILD" && "$PIDFILE" == "$CHILD" ]] && break
  sleep 0.5
done
if [[ -n "$CHILD" && "$PIDFILE" == "$CHILD" ]]; then
  ok "pidfile owned by the daemon child ($PIDFILE)"
else
  bad "pidfile '$PIDFILE' != child pid '$CHILD'"
fi
[[ -f "$FRESH/.sovereign/daemon.lock" ]] && ok "run lock present" || bad "no run lock file"
# The rebrand's canonical config path is ~/.svrnmesh/config.toml; the
# legacy ~/.sovereign/ path also satisfies load(). Accept either.
if [[ -f "$FRESH/.svrnmesh/config.toml" || -f "$FRESH/.sovereign/config.toml" ]]; then
  ok "shared SetupConfig mirrored (first-write path)"
else
  bad "SetupConfig missing — the mirror_to_setup_config first-write regression is back"
fi
# ANSI-safe: grep the enum text, never "field=value".
if grep -aq "source: CliSetup" "$LOG"; then
  ok "relaunched instance resolved CliSetup"
else
  bad "relaunched instance did not resolve CliSetup"
fi

# ── 5: new instance logged the supervised Attach switch ────────────
SWITCHED=""
for i in $(seq 1 30); do
  grep -aq "switching to Attach mode" "$LOG" && { SWITCHED=1; break; }
  sleep 1
done
if [[ -n "$SWITCHED" ]]; then
  ok "relaunched instance switched to supervised Attach"
else
  bad "no 'switching to Attach mode' in log"
fi

echo
echo "[verify] ── result: pass=$PASS fail=$FAIL"
grep -aE "relaunching into the supervised topology|child daemon healthy|supervisor-fallback" "$LOG" | tail -4
exit $([[ $FAIL -eq 0 ]] && echo 0 || echo 1)
