#!/usr/bin/env bash
# First-post-wizard-session relaunch verification (DAEMON_RESILIENCE.md
# P0.1(b)) — the journey-level companion to scripts/daemon-soak.sh.
#
# Drives the REAL desktop binary through the fresh-install wizard flow:
# boots it with a fresh HOME (wizard state), invokes `complete_setup`
# through the command bridge's production IPC path (the same code path
# the wizard's JS uses), then asserts the post-setup relaunch chain. svt-2
# (2026-09-11) deleted supervision, so step 3 changed shape: the daemon is a
# detached SIDECAR, not a child of the app.
#
#   1. complete_setup triggers the relaunch (log line),
#   2. the old instance exits,
#   3. a NEW desktop instance appears, resolves Local{CliSetup}, and a
#      `sovereign-cli-daemon daemon run` sidecar comes up DETACHED,
#   4. that daemon owns :9741 + the pidfile + the flock,
#   5. the new instance logs the Attach switch.
#
# Isolation, per platform:
#   Linux  — self-wraps in `unshare -r -n` (private netns) so :9741/:9745
#            are structurally free and the run can never touch a live
#            daemon or mesh. X11/Wayland still work (filesystem sockets).
#   macOS  — no netns exists. Isolation is a throwaway HOME plus
#            SOVEREIGN_IROH=off, and the port guarantee drops from
#            structural to CHECKED: the drive refuses to start unless
#            :9741 and :9745 are both free. Stop the resident daemon
#            first (`sovereign daemon stop`), or let
#            scripts/desktop-smoke.sh Phase 6 do the handoff for you.
# Force the checked path anywhere with WIZARD_VERIFY_NO_NETNS=1.
#
# Two brief app windows may appear on either platform. Requires the DEBUG
# desktop build (the command bridge is #[cfg(debug_assertions)]) and the
# small soak models.
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

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# ── Isolation: netns on Linux, checked ports elsewhere ───────────────
NETNS=""
if [[ "$(uname -s)" == "Linux" && "${WIZARD_VERIFY_NO_NETNS:-}" != "1" ]]; then
  if [[ "${WIZARD_VERIFY_NS:-}" != "1" ]]; then
    exec unshare -r -n env WIZARD_VERIFY_NS=1 "$0" "$@"
  fi
  ip link set lo up
  NETNS=1
fi

port_live() {  # port_live <port>
  if command -v lsof >/dev/null 2>&1; then
    lsof -nP -iTCP:"$1" -sTCP:LISTEN >/dev/null 2>&1
  else
    curl -sf -m 1 "http://127.0.0.1:$1/v1/models" >/dev/null 2>&1
  fi
}

if [[ -z "$NETNS" ]]; then
  for _p in 9741 9745; do
    if port_live "$_p"; then
      echo "[verify] :$_p is in use, and this host has no netns isolation."
      echo "[verify] This drive needs :9741 and :9745 free. Stop the resident"
      echo "[verify] daemon first:  sovereign daemon stop"
      exit 2
    fi
  done
  echo "[verify] no netns on $(uname -s) — isolation is fresh HOME + free ports (checked)"
fi

BIN="${DESKTOP_BIN:-$REPO_ROOT/target/debug/sovereign-desktop}"

# First-existing-of, because the model tree has moved under this script
# before. The pre-2026-07-28 defaults pointed at $REPO_ROOT/models/<dir>/,
# which stopped existing when the GGUFs moved to sovereign/models/ — so
# the drive exited 2 on every host and, being orphaned, nobody saw it.
# Keep these in step with tests/e2e/real/faults/spawn.ts:28-33.
pick_gguf() {  # pick_gguf <candidate>... — first that exists, else $1
  local c; for c in "$@"; do [[ -f "$c" ]] && { echo "$c"; return 0; }; done
  echo "$1"
}
PRIMARY_GGUF="${PRIMARY_GGUF:-$(pick_gguf \
  "$REPO_ROOT/sovereign/models/Qwen3.5-2B.Q6_K.gguf" \
  "$REPO_ROOT/models/bonsai-8b.gguf/Bonsai-8B-Q1_0.gguf")}"
EMBED_GGUF="${EMBED_GGUF:-$(pick_gguf \
  "$REPO_ROOT/sovereign/models/qwen-embedding-0.6b.gguf" \
  "$REPO_ROOT/sovereign/models/Qwen3-Embedding-0.6B-Q8_0.gguf" \
  "$REPO_ROOT/models/qwen-embedding-0.6b.gguf/Qwen3-Embedding-0.6B-Q8_0.gguf")}"
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

# ── Portable process identity ────────────────────────────────────────
# macOS `ps -E` does NOT expose another process's environment (SIP), so
# the old "scan /proc/<pid>/environ for HOME=$FRESH" cannot work there.
# A pre-run snapshot is portable and strictly stronger: anything that
# appears afterwards is ours by construction, and we additionally assert
# the parent/child link rather than mere co-existence.
PRE_PIDS=" $(pgrep -f "sovereign-desktop" 2>/dev/null | tr '\n' ' ') "
new_desktop_pids() {
  local p out=""
  for p in $(pgrep -f "sovereign-desktop" 2>/dev/null); do
    [[ "$PRE_PIDS" == *" $p "* ]] || out+="$p "
  done
  echo "$out"
}
proc_cmd()  { ps -o command= -p "$1" 2>/dev/null; }
proc_ppid() { ps -o ppid= -p "$1" 2>/dev/null | tr -d '[:space:]'; }
# svt-2: the daemon is no longer the app's CHILD, it is a detached sibling.
# Its own process group is what proves the detachment, so read pgid too.
proc_pgid() { ps -o pgid= -p "$1" 2>/dev/null | tr -d '[:space:]'; }

PASS=0; FAIL=0
ok()  { PASS=$((PASS+1)); echo "  ✓ $*"; }
bad() { FAIL=$((FAIL+1)); echo "  ✘ $*"; }

declare -a KILL_PIDS=()
cleanup() {
  local p
  for p in "${KILL_PIDS[@]:-}"; do [[ -n "$p" ]] && kill -9 "$p" 2>/dev/null; done
  # Belt-and-braces: kill anything that APPEARED during this run (never
  # pkill by name — a live desktop may run the same binary, and on macOS
  # we cannot read its env to tell them apart).
  for p in $(new_desktop_pids); do kill -9 "$p" 2>/dev/null; done
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
# svt-2 (2026-09-11): the desktop no longer supervises anything, so the
# relaunch is about the CONFIG being read at startup, not about entering a
# supervised topology. `setup_flow::relaunch_after_setup` replaced
# `supervisor_setup::maybe_restart_into_supervised`; the log line moved with it.
if grep -aq "relaunching so the new config is read at startup" "$LOG"; then
  ok "app1 logged the post-setup relaunch"
else
  bad "no 'relaunching so the new config is read at startup' in app1 log"
fi

# ── 3: new instance + the SIDECAR daemon appear ────────────────────
# svt-2 deleted `--daemon-child`: `main.rs` now refuses every daemon argv
# form with NOT_A_DAEMON, and the daemon this app talks to is the Tauri
# SIDECAR — a separate `sovereign-cli-daemon` binary running `daemon run`,
# brought up by `serving_host::ensure_reachable`. So the desktop-pid scan no
# longer finds it: look for the sidecar by its own name.
# Identity by pre-snapshot difference + cmdline (portable), never by
# /proc/<pid>/environ — see PRE_PIDS above.
APP2=""; CHILD=""
for i in $(seq 1 90); do
  for p in $(new_desktop_pids); do
    [[ "$p" == "$APP1" ]] && continue
    APP2="$p"
  done
  for p in $(pgrep -f "sovereign-cli-daemon" 2>/dev/null); do
    proc_cmd "$p" | grep -q -- "daemon run" && CHILD="$p"
  done
  [[ -n "$APP2" && -n "$CHILD" ]] && break
  sleep 1
done
[[ -n "$APP2" ]] && KILL_PIDS+=("$APP2")
[[ -n "$CHILD" ]] && KILL_PIDS+=("$CHILD")
if [[ -n "$APP2" ]]; then ok "relaunched desktop instance running (pid $APP2)"; else bad "no relaunched desktop instance found"; fi
if [[ -n "$CHILD" ]]; then ok "sidecar daemon running (pid $CHILD)"; else bad "no sovereign-cli-daemon 'daemon run' process found"; fi
# THE ASSERTION INVERTED AT svt-2, and the inversion is the point. It used to
# demand the daemon be PARENTED by the app, which is what "supervised" meant.
# Now the requirement is the opposite: the daemon must be DETACHED, in its own
# process group (`process_group(0)` in sovereign-turn-client/src/reach.rs), so
# that closing the window leaves the node serving the mesh. A daemon sharing
# the app's process group would die with it — the regression this checks for.
if [[ -n "$APP2" && -n "$CHILD" ]]; then
  CHILD_PGID="$(proc_pgid "$CHILD")"
  APP2_PGID="$(proc_pgid "$APP2")"
  if [[ -n "$CHILD_PGID" && "$CHILD_PGID" != "$APP2_PGID" ]]; then
    ok "sidecar daemon is detached (pgid $CHILD_PGID != app pgid $APP2_PGID)"
  else
    bad "daemon pgid=$CHILD_PGID shares the app's group $APP2_PGID — it would die with the window"
  fi
fi

# ── 4: child owns :9741 + pidfile + flock + mirrored config ────────
READY=""
for i in $(seq 1 120); do
  curl -sf -m 2 http://127.0.0.1:9741/v1/models >/dev/null 2>&1 && { READY=1; break; }
  sleep 1
done
if [[ -n "$READY" ]]; then ok ":9741 serving (sidecar daemon)"; else bad ":9741 never answered"; fi
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
# The run lock lives in the DATA ROOT (re-keyed off $HOME 2026-08-24), which on
# a fresh wizard run is the branded dir — or the legacy one on a machine the
# rebrand fallback picked. Accept either, same as the config check below.
if [[ -f "$FRESH/.svrnmesh/daemon.lock" || -f "$FRESH/.sovereign/daemon.lock" ]]; then
  ok "run lock present"
else
  bad "no run lock file"
fi
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
