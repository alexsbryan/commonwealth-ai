#!/usr/bin/env bash
# desktop-smoke.sh — best-ROI desktop-UX-regression smoke for Sovereign.
#
# Answers ONE question in <=4h on a dev machine: "did the desktop app's user
# experience regress?" It does NOT chase absolute quality — it detects a DELTA
# against committed/captured baselines across the five UX-regression classes:
#   render/FSM · broken-flow · trust(grounding/citations/safety) · routing · perf
#
# Design (why it's shaped this way):
#   * Fail-fast, cheap->expensive. Phase 0 (no model) HARD-STOPS the run — no
#     point loading a 30GB model if the app won't compile or a panel won't render.
#   * Delta-vs-baseline, not absolute thresholds. Every CLI lane ships a committed
#     baseline (bench gate); perf uses tolerance bands (MoE + HW jitter move things
#     +/-, so exact-match would be flaky).
#   * ONE model held constant (the shipped 35B), judge calibrated FIRST. A smoke
#     measures a delta, so consistency beats absolute accuracy — no 122B swap.
#   * Serialize model-heavy phases. A model-loaded Playwright run concurrent with a
#     full cargo build is the OOM hazard here; phases never overlap.
#   * Desktop-authentic layers on top of the daemon-level lanes: the bridge-parity
#     probe (routes through the REAL Tauri command handlers on :9745) and the
#     real-mode invariant pack (drives the actual sovereign-desktop binary).
#
# Phases (soft budgets, tunable via SMOKE_P<n>_SECS env). Executed order groups
# by daemon topology — 1,2,3,5 share the resident daemon on :9741, then 4 runs
# after them because it owns its OWN hermetic :9741 daemon (managed real-mode),
# and 6 runs last inside a private netns (it needs no port handoff at all):
#   0  static & render     ~30m  lint(compile) + svelte-check + vitest + synthetic e2e + desktop unit tests   [HARD STOP]
#   1  perf baseline       ~10m  daemon surface + throughput_probe x2 slots + mtp accept-rate + TTFI
#   2  daemon quality      ~25m  inner-chaos --calibrate (gate judge) + sovereign-ci-bench.sh --quick
#   3  desktop-layer       ~20m  routing-replay through the command bridge (:9745) vs the direct baseline
#                                (bridge desktop launched with naked_mode=false so routing is engaged)
#   5  safety soak    reserves-4  eval inner-chaos --minutes <remaining, minus Phase-4 reserve>
#   4  real-binary e2e     ~50m  MANAGED real-mode: frees :9741, runs test:e2e:real + test:e2e:faults
#                                against SOVEREIGN_REAL_CHAT_MODEL, then restores the resident daemon
#   6  production boot     ~15m  scripts/wizard-verify.sh — fresh wizard -> complete_setup -> supervised
#                                relaunch -> `current_exe() --daemon-child` -> Attach. The ONLY lane that
#                                reaches that branch; everything else that supervises pins
#                                SOVEREIGN_CLI_PATH. Linux isolates via netns; macOS/other frees :9741
#                                around the drive (same handoff as phase 4) and restores it after.
#
# Usage:
#   scripts/desktop-smoke.sh [--budget-secs N] [--quick] [--capture-baseline]
#                            [--skip 0,3,4] [--only 2] [--build] [--no-daemon-manage]
#                            [--dry-run]
#
# Exit: 0 = all executed phases within tolerance; 1 = a regression/gate failed;
#       2 = hard-stop (Phase 0) or setup error. SKIPPED phases never fail the run
#       but are always reported (no silent gaps).

set -uo pipefail

# ── Paths & constants ────────────────────────────────────────────────────────
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"
DESKTOP_DIR="$REPO_ROOT/sovereign/crates/sovereign-desktop"
CLI="$REPO_ROOT/target/debug/sovereign-cli-llm"
# The DISPATCHER, not the sibling: `quality check` is in-process in
# `sovereign-cli` and needs its `dev-tools` feature.
SVRN="$REPO_ROOT/target/debug/sovereign-cli"
DESKTOP_BIN="$REPO_ROOT/target/debug/sovereign-desktop"
# NOTE: this is a real filename on this host, not a family name — the `_XL`
# suffix is load-bearing. It was missing until 2026-08-04, which cost an
# overnight run (see ensure_target_primary). ensure_config_primary now
# refuses a path that is not on disk, so a future rename fails loudly at the
# bounce instead of bricking the operator's daemon.
SHIPPED_PRIMARY="$REPO_ROOT/sovereign/models/Qwen3.6-35B-A3B-MTP-UD-Q6_K_XL.gguf"
DAEMON_URL="http://localhost:9741"
BRIDGE_PORT=9745
BRIDGE_URL="http://127.0.0.1:${BRIDGE_PORT}"
SVR="$HOME/.svrnmesh"

STAMP="$(date +%Y%m%d-%H%M%S)"
ART="$REPO_ROOT/test-artifacts/desktop-smoke/$STAMP"
BASELINE="$REPO_ROOT/test-artifacts/desktop-smoke/baseline"
mkdir -p "$ART" "$BASELINE"

# ── Defaults / args ──────────────────────────────────────────────────────────
BUDGET_SECS=14400          # 4h overall
QUICK=""
CAPTURE_BASELINE=""
SKIP=""
ONLY=""
DO_BUILD=""
MANAGE_DAEMON="1"
DRY_RUN=""
CONTINUE=""                # --continue: Phase 0 failures record but don't hard-stop
TARGET_PRIMARY="$SHIPPED_PRIMARY"   # --primary <path> overrides (e.g. run the 2B baseline)
: "${SMOKE_P0_SECS:=1800}"
: "${SMOKE_P1_SECS:=600}"
: "${SMOKE_P2_SECS:=1500}"
: "${SMOKE_P3_SECS:=1200}"
: "${SMOKE_P4_SECS:=3000}"
# P5 consumes whatever budget remains (min SMOKE_P5_MIN_SECS).
: "${SMOKE_P5_MIN_SECS:=600}"
: "${SMOKE_P6_SECS:=900}"
# The perf tolerance bands (PERF_TPS_DROP_PCT / PERF_TTFT_RISE_PCT) moved into
# `sovereign/bench/quality-check/throughput.toml` as pre-registered per-stem
# BARS, which is the difference between a threshold written down before the
# run and one an env var can move after it.
: "${SAFETY_DROP_ABS:=0.05}"     # safety_number may not drop >0.05

while [ $# -gt 0 ]; do
  case "$1" in
    --budget-secs) BUDGET_SECS="$2"; shift 2 ;;
    --quick) QUICK="1"; shift ;;
    --capture-baseline) CAPTURE_BASELINE="1"; shift ;;
    --skip) SKIP="$2"; shift 2 ;;
    --only) ONLY="$2"; shift 2 ;;
    --build) DO_BUILD="1"; shift ;;
    --no-daemon-manage) MANAGE_DAEMON=""; shift ;;
    --primary) TARGET_PRIMARY="$2"; shift 2 ;;
    --continue) CONTINUE="1"; shift ;;
    --dry-run) DRY_RUN="1"; shift ;;
    -h|--help) sed -n '2,50p' "$0"; exit 0 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

START_EPOCH="$(date +%s)"
declare -a ROWS=()   # "phase|status|secs|detail"
OVERALL_RC=0
BRIDGE_PID=""

# quick-mode shrinks the sampling knobs (a smoke wants signal, not precision)
if [ -n "$QUICK" ]; then ROUTE_LIMIT=10; else ROUTE_LIMIT=20; fi
# capturing a baseline should traverse every phase, so Phase 0 must not hard-stop.
[ -n "$CAPTURE_BASELINE" ] && CONTINUE="1"

# ── Helpers ──────────────────────────────────────────────────────────────────
log()   { printf '\033[1;36m[smoke %s]\033[0m %s\n' "$(date +%H:%M:%S)" "$*"; }
warn()  { printf '\033[1;33m[smoke WARN]\033[0m %s\n' "$*" >&2; }
err()   { printf '\033[1;31m[smoke ERR ]\033[0m %s\n' "$*" >&2; }

elapsed() { echo $(( $(date +%s) - START_EPOCH )); }
remaining() { echo $(( BUDGET_SECS - $(elapsed) )); }

record() { ROWS+=("$1|$2|$3|$4"); [ "$2" = "FAIL" ] && OVERALL_RC=1; return 0; }

phase_enabled() {
  local n="$1"
  [ -n "$ONLY" ] && { [[ ",$ONLY," == *",$n,"* ]] && return 0 || return 1; }
  [ -n "$SKIP" ] && [[ ",$SKIP," == *",$n,"* ]] && return 1
  return 0
}

wait_daemon() {  # wait_daemon <timeout_secs>
  local t="${1:-90}" i=0
  while [ "$i" -lt "$t" ]; do
    curl -s --max-time 3 "$DAEMON_URL/v1/models" >/dev/null 2>&1 && return 0
    sleep 3; i=$((i+3))
  done
  return 1
}

daemon_primary() {
  curl -s --max-time 5 "$DAEMON_URL/v1/models" 2>/dev/null \
    | python3 -c 'import json,sys
try:
    d=json.load(sys.stdin); print([m["owned_by"] for m in d["data"] if m["id"]=="primary"][0])
except Exception: print("")' 2>/dev/null
}

ensure_target_primary() {
  # Bounce the resident daemon onto the target primary (default 35B, or whatever
  # --primary passed) if not already loaded. Uses the supervised-daemon contract:
  # any exit auto-restarts unless the stop sentinel exists, so edit config +
  # SIGTERM = clean model swap.
  [ -z "$MANAGE_DAEMON" ] && { log "daemon-manage off; using resident daemon as-is"; return 0; }
  local want; want="$(basename "$TARGET_PRIMARY" .gguf)"
  local cur; cur="$(daemon_primary)"
  if [[ "$cur" == *"$want"* ]]; then log "daemon already on target primary ($want)"; return 0; fi
  log "bouncing daemon onto target primary '$want' (was: ${cur:-down})"
  # ONE decider for "rewrite config.toml's primary=" — ensure_config_primary.
  # This used to be a second, unguarded inline copy of that rewrite, and on
  # 2026-08-03 it repointed the operator's LIVE config at a GGUF that is not
  # on disk (SHIPPED_PRIMARY was missing its `_XL` suffix), SIGTERM'd the
  # daemon, and left the workstation with no daemon for ~14h. Phases 1,2,3,5
  # and the two downstream overnight blocks then "failed" against a dead
  # port. The existence check lived in ensure_config_primary the whole time
  # — it was simply not on this path (ARCH_PRINCIPLES §10.6, §15 "two
  # implementations of one operation").
  #
  # A missing model is NOT a degraded run: refuse the bounce and leave the
  # resident daemon exactly as found. Return 2 so the caller can record a
  # precondition SKIP rather than a product FAIL.
  if ! ensure_config_primary "$TARGET_PRIMARY"; then
    err "refusing to bounce onto a primary that is not on disk: $TARGET_PRIMARY"
    err "leaving the resident daemon untouched (still on: ${cur:-down})"
    return 2
  fi
  rm -f "$SVR/supervised.stop"
  local pid; pid="$(pgrep -f 'sovereign-cli-daemon daemon run' | head -1)"
  [ -n "$pid" ] && kill -TERM "$pid" 2>/dev/null
  wait_daemon 150 || { err "daemon did not return after bounce"; return 1; }
  log "daemon back up; primary -> $(daemon_primary)"
}

# ── Managed real-mode needs :9741 free (the harness spawns its OWN hermetic
# daemon there — hardcoded port + a startup guard that throws if it's busy).
# These two helpers hand :9741 off to the harness for Phase 4 and give it back
# to the resident supervisor afterward, leaving the machine as found.
stop_resident_daemon() {
  [ -z "$MANAGE_DAEMON" ] && { warn "daemon-manage off — cannot free :9741 for managed real-mode"; return 1; }
  log "  freeing :9741 — stopping resident daemon (sovereign daemon stop)"
  # Belt-and-suspenders: the sentinel keeps a supervisor (if one is running)
  # from racing a restart; `sovereign daemon stop` cleanly stops the daemon
  # process itself (works whether it's supervised or a bare CLI-started one).
  touch "$SVR/supervised.stop"
  run_capped 60 sovereign daemon stop > "$ART/p4-daemon-stop.log" 2>&1 || true
  local i=0
  while [ "$i" -lt 60 ]; do
    curl -s --max-time 2 "$DAEMON_URL/v1/models" >/dev/null 2>&1 || { log "  :9741 free"; return 0; }
    sleep 2; i=$((i+2))
  done
  # Return FAILURE. This used to `return 0`, so callers proceeded into a
  # run whose port guard was guaranteed to trip and reported it as a
  # product FAIL instead of an environment SKIP. A lane that could not get
  # its preconditions did not verify anything — say so.
  warn "  :9741 still answering after 60s — cannot free the port"
  return 1
}

# Restore the resident daemon after managed real-mode. `sovereign daemon start`
# handles the GPU/toolbox launch itself and works from this (non-toolbox)
# context — unlike a raw `toolbox run`, which needs flatpak-spawn we don't have.
# The restored daemon is UNSUPERVISED (no RSS-guard auto-restart); that's the
# only option reachable from here, so we say so loudly.
restore_resident_daemon() {
  [ -z "$MANAGE_DAEMON" ] && return 0
  rm -f "$SVR/supervised.stop"
  # re-point config at the smoke's target primary so the restart loads it
  ensure_config_primary "$TARGET_PRIMARY"
  log "  restoring resident daemon (sovereign daemon start — UNSUPERVISED)"
  run_capped 150 sovereign daemon start > "$ART/p4-daemon-restore.log" 2>&1 || true
  wait_daemon 30 \
    && log "  resident daemon back up (primary $(daemon_primary)); note: unsupervised — re-run your supervisor in the toolbox to restore the RSS guard" \
    || warn "  resident daemon didn't return — restart it yourself: sovereign daemon start"
}

# Rewrite config.toml's primary= line to $1 (idempotent; backs up once).
#
# REFUSES a path that does not exist. This writes the OPERATOR'S live
# ~/.svrnmesh/config.toml, and a primary that isn't on disk is not a
# degraded run — the daemon will not start at all: the VRAM preflight
# stats the file, the error underflows the size to ~i64::MAX MiB, and the
# gate rejects the whole config. The smoke then reports "resident daemon
# didn't return" and leaves the box without a daemon. Observed 2026-07-28
# on darwin, where SHIPPED_PRIMARY names a GGUF that is not present.
# Leaving the existing primary alone is always the safer failure.
ensure_config_primary() {
  local model="$1"
  if [ ! -e "$model" ]; then
    warn "  refusing to repoint primary at a missing file: $model"
    warn "  leaving the existing primary in place (config not touched)"
    return 1
  fi
  grep -q "^primary = \"$model\"" "$SVR/config.toml" 2>/dev/null && return 0
  cp "$SVR/config.toml" "$SVR/config.toml.bak-smoke-$STAMP" 2>/dev/null || true
  python3 - "$SVR/config.toml" "$model" <<'PY'
import sys,re
p,model=sys.argv[1],sys.argv[2]
s=open(p).read()
s=re.sub(r'(?m)^primary = ".*"$', f'primary = "{model}"', s, count=1)
open(p,"w").write(s)
PY
}

cleanup() {
  [ -n "$BRIDGE_PID" ] && kill "$BRIDGE_PID" 2>/dev/null || true
}
trap cleanup EXIT

# run_capped <secs> <cmd...> ; returns cmd's rc (124 on timeout)
# GNU coreutils `timeout` does not exist on macOS. Until 2026-07-28 this
# was a bare `timeout ...`, so on darwin EVERY phase exited 127 — including
# `stop_resident_daemon`'s own `run_capped 60 sovereign daemon stop`, whose
# `|| true` then swallowed it, so the daemon was never even asked to stop
# and the port poll blamed the daemon. One missing binary, two misleading
# symptoms. Resolve the strategy once, announce it, and fall back.
TIMEOUT_BIN=""
if command -v timeout >/dev/null 2>&1; then TIMEOUT_BIN="timeout"
elif command -v gtimeout >/dev/null 2>&1; then TIMEOUT_BIN="gtimeout"; fi

run_capped() {
  local cap="$1"; shift
  if [ -n "$TIMEOUT_BIN" ]; then "$TIMEOUT_BIN" --preserve-status "$cap" "$@"; return $?; fi
  # Portable fallback (bash 3.2 — macOS ships no newer): background the
  # command, poll, TERM then KILL on expiry. Returns the command's own
  # status, or 124 when we killed it, matching coreutils' convention.
  # Like `timeout` itself, this signals the direct child only.
  "$@" &
  local cmd_pid=$! waited=0 grace=0
  while kill -0 "$cmd_pid" 2>/dev/null; do
    if [ "$waited" -ge "$cap" ]; then
      kill -TERM "$cmd_pid" 2>/dev/null
      grace=0
      while [ "$grace" -lt 5 ] && kill -0 "$cmd_pid" 2>/dev/null; do sleep 1; grace=$((grace+1)); done
      kill -KILL "$cmd_pid" 2>/dev/null
      wait "$cmd_pid" 2>/dev/null
      return 124
    fi
    sleep 1; waited=$((waited+1))
  done
  wait "$cmd_pid"
}

# ── Phase 0: static & render (no model) — HARD STOP ──────────────────────────
phase0() {
  phase_enabled 0 || { record "0 static/render" "SKIP" 0 "disabled"; return 0; }
  log "PHASE 0 — static & render (compile + svelte-check + vitest + synthetic e2e + desktop unit)"
  local t0; t0=$(date +%s) fail=0 detail=""
  [ -n "$DRY_RUN" ] && { record "0 static/render" "DRY" 0 "would run lint + npm check/test/e2e + desktop unit"; return 0; }

  run_capped 900 scripts/sovereign-lint.sh --human > "$ART/p0-lint.log" 2>&1 \
    && log "  lint: PASS" || { fail=1; detail+="lint "; err "  lint FAILED (see p0-lint.log)"; }

  ( cd "$DESKTOP_DIR" && run_capped 180 npm run check ) > "$ART/p0-svelte-check.log" 2>&1 \
    && log "  svelte-check: PASS" || { fail=1; detail+="svelte-check "; err "  svelte-check FAILED"; }

  ( cd "$DESKTOP_DIR" && run_capped 180 npm run test ) > "$ART/p0-vitest.log" 2>&1 \
    && log "  vitest: PASS" || { fail=1; detail+="vitest "; err "  vitest FAILED"; }

  ( cd "$DESKTOP_DIR" && run_capped 420 npm run test:e2e ) > "$ART/p0-e2e-synth.log" 2>&1 \
    && log "  synthetic e2e: PASS" || { fail=1; detail+="synth-e2e "; err "  synthetic e2e FAILED"; }

  run_capped 600 scripts/sovereign-test.sh --human --package sovereign-desktop > "$ART/p0-desktop-unit.log" 2>&1 \
    && log "  desktop unit: PASS" || { fail=1; detail+="desktop-unit "; err "  desktop unit FAILED"; }

  local secs=$(( $(date +%s) - t0 ))
  if [ "$fail" -eq 0 ]; then record "0 static/render" "PASS" "$secs" "compile+render+unit clean"
  else
    record "0 static/render" "FAIL" "$secs" "${detail}"
    if [ -n "$CONTINUE" ]; then warn "PHASE 0 failed but --continue set — proceeding (baseline mode)"
    else err "PHASE 0 failed — HARD STOP (fix before the model-heavy phases)"; print_scoreboard; exit 2; fi
  fi
}

# ── Phase 1: perf baseline ───────────────────────────────────────────────────
phase1() {
  phase_enabled 1 || { record "1 perf" "SKIP" 0 "disabled"; return 0; }
  log "PHASE 1 — perf baseline (throughput + TTFT + MTP + TTFI)"
  [ -n "$DRY_RUN" ] && { record "1 perf" "DRY" 0 "throughput_probe x2 + mtp-probe + ttfi"; return 0; }
  local t0; t0=$(date +%s) fail=0 detail=""
  sovereign/scripts/smoke-attach-mode.sh > "$ART/p1-attach.log" 2>&1 \
    && log "  daemon surface: up" || { warn "  smoke-attach probes failed (non-fatal)"; detail+="attach? "; }

  # The throughput lane of `svrn quality check` owns this now. It runs the
  # same `scripts/throughput_probe.py`, over four declared arms instead of
  # two, against PRE-REGISTERED bars in `sovereign/bench/quality-check/
  # throughput.toml` and a per-stack baseline that is committed — where the
  # comparison this block used to do read a gitignored `$BASELINE` directory
  # that does not exist on a fresh checkout, so on this host it captured on
  # every run and compared on none of them.
  if run_capped 400 "$SVRN" quality check --lane throughput > "$ART/p1-throughput.log" 2>&1; then
    log "  throughput lane: PASS"; detail+="throughput:pass "
  else
    fail=1; detail+="throughput:fail "; err "  throughput lane failed — see $ART/p1-throughput.log"
  fi

  run_capped 180 scripts/mtp-probe.sh --n 5 --max-tokens 200 > "$ART/p1-mtp.log" 2>&1 \
    && log "  mtp accept-rate: recorded" || warn "  mtp-probe failed (non-fatal)"

  ( cd "$DESKTOP_DIR" && run_capped 300 npm run test:ttfi ) > "$ART/p1-ttfi.log" 2>&1 \
    && { log "  TTFI: PASS"; } || { fail=1; detail+="ttfi "; err "  TTFI regressed/failed"; }

  local secs=$(( $(date +%s) - t0 ))
  [ "$fail" -eq 0 ] && record "1 perf" "PASS" "$secs" "$detail" || record "1 perf" "FAIL" "$secs" "$detail"
}

# ── Phase 2: daemon quality lanes ────────────────────────────────────────────
phase2() {
  phase_enabled 2 || { record "2 quality" "SKIP" 0 "disabled"; return 0; }
  log "PHASE 2 — daemon quality (calibrate judge + sovereign-ci-bench --quick)"
  [ -n "$DRY_RUN" ] && { record "2 quality" "DRY" 0 "inner-chaos --calibrate + ci-bench --quick"; return 0; }
  local t0; t0=$(date +%s) fail=0 detail=""

  run_capped 300 "$CLI" eval inner-chaos --calibrate > "$ART/p2-calibrate.log" 2>&1 \
    && log "  judge calibration: PASS" \
    || { fail=1; detail+="judge-calibration "; err "  judge calibration below floor — safety numbers untrustworthy"; }

  # NB: never pass --update-baseline here. ci-bench manages its OWN committed
  # per-lane baselines (the 35B-era CI references) — our --capture-baseline is
  # about the smoke's own perf/safety refs, a separate system. A 2B run WILL
  # fail lanes vs the committed baselines; that quantifies the model gap and is
  # expected, not a script fault.
  # ci-bench compares against its OWN committed per-lane baselines, which may be
  # from a different/stronger config than the smoke's primary. Below-baseline is
  # informational (it quantifies the model gap), NOT a smoke failure — only the
  # judge-calibration gate above is safety-critical here. Record it as a WARN so
  # it stays visible without flipping the whole run to NO-GO.
  run_capped "$SMOKE_P2_SECS" scripts/sovereign-ci-bench.sh --quick --report "$ART/ci-bench" \
    > "$ART/p2-ci-bench.log" 2>&1 \
    && { log "  ci-bench: PASS"; detail+="ci-bench:pass"; } \
    || { detail+="ci-bench:below-baseline(warn)"; warn "  ci-bench lanes below committed baseline (informational — expected on a weaker/different model)"; }

  local secs=$(( $(date +%s) - t0 ))
  [ "$fail" -eq 0 ] && record "2 quality" "PASS" "$secs" "$detail" || record "2 quality" "FAIL" "$secs" "$detail"
}

# ── Phase 3: desktop-layer isolation (bridge) ────────────────────────────────
phase3() {
  phase_enabled 3 || { record "3 desktop-layer" "SKIP" 0 "disabled"; return 0; }
  log "PHASE 3 — desktop-layer (routing-replay through the command bridge :$BRIDGE_PORT)"
  [ -n "$DRY_RUN" ] && { record "3 desktop-layer" "DRY" 0 "launch bridge + bench routing-replay vs direct"; return 0; }
  [ -x "$DESKTOP_BIN" ] || { record "3 desktop-layer" "SKIP" 0 "desktop binary missing (run --build)"; warn "  no $DESKTOP_BIN — skipping"; return 0; }
  local t0; t0=$(date +%s) bank="sovereign/bench/routing/cells_v1.toml"

  # Routing-replay only measures routing if the ROUTER is engaged. `naked_mode`
  # (a desktop.toml setting the resident config has ON) bypasses routing and
  # affordances entirely → 0/10 `provenance.intent`. Launch the bridge desktop
  # against a scratch XDG_CONFIG_HOME cloned from the real config with naked_mode
  # flipped OFF, so we exercise routing without mutating the user's config.
  local cfgroot="$ART/p3-xdg"; mkdir -p "$cfgroot/sovereign"
  if [ -f "$HOME/.config/sovereign/desktop.toml" ]; then
    cp "$HOME/.config/sovereign/desktop.toml" "$cfgroot/sovereign/desktop.toml"
    python3 - "$cfgroot/sovereign/desktop.toml" <<'PY'
import sys,re
p=sys.argv[1]; s=open(p).read()
if re.search(r'(?m)^\s*naked_mode\s*=', s):
    s=re.sub(r'(?m)^\s*naked_mode\s*=.*$', 'naked_mode = false', s)
else:
    s=s.rstrip()+'\nnaked_mode = false\n'
open(p,'w').write(s)
PY
  else
    printf 'naked_mode = false\n' > "$cfgroot/sovereign/desktop.toml"
  fi

  XDG_CONFIG_HOME="$cfgroot" SOVEREIGN_COMMAND_BRIDGE=1 SOVEREIGN_COMMAND_BRIDGE_PORT="$BRIDGE_PORT" \
    "$DESKTOP_BIN" > "$ART/p3-desktop.log" 2>&1 &
  BRIDGE_PID=$!
  local i=0 up=""
  while [ "$i" -lt 60 ]; do curl -s --max-time 2 "$BRIDGE_URL/healthz" >/dev/null 2>&1 && { up=1; break; }; sleep 2; i=$((i+2)); done
  if [ -z "$up" ]; then record "3 desktop-layer" "SKIP" "$(( $(date +%s)-t0 ))" "bridge :$BRIDGE_PORT never came up (headless? build?)"; warn "  bridge down — skipping"; kill "$BRIDGE_PID" 2>/dev/null; BRIDGE_PID=""; return 0; fi
  log "  bridge up (pid $BRIDGE_PID)"

  run_capped "$SMOKE_P3_SECS" "$CLI" bench routing-replay --bank "$bank" --bridge-url "$BRIDGE_URL" \
    --limit "$ROUTE_LIMIT" --out "$ART/p3-routing-bridge.json" > "$ART/p3-routing.log" 2>&1
  local rc=$?
  kill "$BRIDGE_PID" 2>/dev/null; BRIDGE_PID=""
  local secs=$(( $(date +%s) - t0 ))
  if [ "$rc" -eq 0 ]; then
    local acc; acc=$(python3 -c 'import json,sys
try: print(json.load(open(sys.argv[1])).get("accuracy","?"))
except Exception: print("?")' "$ART/p3-routing-bridge.json" 2>/dev/null)
    record "3 desktop-layer" "PASS" "$secs" "bridge routing acc=$acc (vs P2 direct routing baseline)"
    log "  desktop-bridge routing accuracy=$acc"
  else
    record "3 desktop-layer" "FAIL" "$secs" "routing-replay through bridge errored (rc=$rc)"
  fi
}

# ── Phase 4: real-binary end-to-end (invariant pack + faults) ────────────────
phase4() {
  phase_enabled 4 || { record "4 real-e2e" "SKIP" 0 "disabled"; return 0; }
  log "PHASE 4 — real-binary e2e (MANAGED hermetic daemon, invariant pack + faults)"
  [ -n "$DRY_RUN" ] && { record "4 real-e2e" "DRY" 0 "free :9741 → npm test:e2e:real + test:e2e:faults (managed) → restore daemon"; return 0; }
  [ -x "$DESKTOP_BIN" ] || { record "4 real-e2e" "SKIP" 0 "desktop binary missing (run --build)"; return 0; }
  local t0 fail=0 detail=""; t0=$(date +%s)
  # auto-xvfb if headless — LINUX ONLY. Xvfb does not exist on darwin, where
  # DISPLAY/WAYLAND_DISPLAY are always empty, so the bare emptiness test
  # asked macOS to run under a display server it has no way to provide.
  local xvfb=""
  [ "$(uname -s)" = "Linux" ] && [ -z "${DISPLAY:-}${WAYLAND_DISPLAY:-}" ] && xvfb="1"

  # Real-mode's governance overlay HARD-assumes the daemon's data dir == the
  # harness's scratch HOME (global-setup.ts:285), and the faults suite spins its
  # OWN supervised child daemon — both are MANAGED-mode invariants. So free :9741
  # and let the harness own a hermetic daemon. Critically we do NOT force the
  # smoke's 28GB primary here: real-mode/faults test UX invariants + kill/recovery
  # MECHANICS, not model quality. A heavy primary makes the managed daemon's cold
  # load blow the faults supervisor's heartbeat window (observed 2026-07-15:
  # "daemon stopped responding, 3 failed heartbeats"). Let both use the harness
  # DEFAULT small model — fast, hermetic, deterministic.
  if ! stop_resident_daemon; then
    record "4 real-e2e" "SKIP" "$(( $(date +%s)-t0 ))" "could not free :9741 (--no-daemon-manage)"; return 0
  fi

  # `${xvfb:+export FOO=1;}` DOES NOT WORK: the `;` is inside the parameter
  # expansion, so it is a WORD, not a command separator. The line collapsed to
  # a single `export` command that swallowed `run_capped 3000 npm run
  # test:e2e:real` as its arguments — p4-real.log contained nothing but
  # "export: `3000': not a valid identifier" and npm never ran. Because
  # `xvfb` is set whenever DISPLAY and WAYLAND_DISPLAY are both empty (always
  # true on darwin), this phase had NEVER executed on this host while
  # reporting a product FAIL every time. Assign the env inline on the command
  # instead — no subshell `export`, no separator to lose.
  (
    cd "$DESKTOP_DIR" || exit 1
    [ -n "$xvfb" ] && export SOVEREIGN_REAL_XVFB=1
    run_capped "$SMOKE_P4_SECS" npm run test:e2e:real
  ) > "$ART/p4-real.log" 2>&1 \
    && log "  real-mode invariant pack: PASS" \
    || { fail=1; detail+="invariant-pack "; err "  real-mode e2e FAILED (see p4-real.log)"; }

  if [ "$(remaining)" -gt 900 ]; then
    (
      cd "$DESKTOP_DIR" || exit 1
      [ -n "$xvfb" ] && export SOVEREIGN_REAL_XVFB=1
      run_capped 1200 npm run test:e2e:faults
    ) > "$ART/p4-faults.log" 2>&1 \
      && log "  fault suite: PASS" \
      || { fail=1; detail+="faults "; err "  fault suite FAILED"; }
  else
    detail+="faults:skipped(budget) "; warn "  skipping fault suite — low budget"
  fi

  restore_resident_daemon   # bring :9741 back via the CLI (unsupervised — logged)

  local secs=$(( $(date +%s) - t0 ))
  [ "$fail" -eq 0 ] && record "4 real-e2e" "PASS" "$secs" "${detail:-invariants+faults clean}" || record "4 real-e2e" "FAIL" "$secs" "$detail"
}

# ── Phase 6: the production boot chain ───────────────────────────────────────
# Every OTHER supervised lane in the repo pins SOVEREIGN_CLI_PATH
# (faults/spawn.ts:151, tests/e2e/scripts/lib/harness.mjs:257), which sends
# supervisor_setup::resolve_daemon_child() down branch 1 — "point at a CLI
# build". A packaged install has no such env var and takes branch 2,
# `current_exe() --daemon-child` (supervisor_setup.rs:74-78). wizard-verify.sh
# unsets the var, so it is the ONLY thing that exercises the branch every
# shipped user runs. It was orphaned until 2026-07-28: referenced by
# DAEMON_RESILIENCE.md and by nothing executable, despite catching a real
# ship-blocking bug on its first run (fresh desktop-only installs never wrote
# config.toml, so supervision would never have engaged for them).
#
# It self-wraps in `unshare -r -n`, so it owns a private netns and never
# contends for :9741 — hence no port handoff here, and it is safe to run after
# phase 4 has restored the resident daemon.
phase6() {
  phase_enabled 6 || { record "6 prod-boot" "SKIP" 0 "disabled"; return 0; }
  log "PHASE 6 — production boot chain (fresh wizard → complete_setup → --daemon-child → Attach)"
  [ -n "$DRY_RUN" ] && { record "6 prod-boot" "DRY" 0 "scripts/wizard-verify.sh in a private netns"; return 0; }
  [ -x "$DESKTOP_BIN" ] || { record "6 prod-boot" "SKIP" 0 "desktop binary missing (run --build)"; return 0; }
  # Linux runs wizard-verify inside a private netns, so :9741/:9745 are
  # structurally free and no handoff is needed. Everywhere else the drive
  # enforces free ports itself and refuses to start otherwise — so free
  # them the same way phase 4 does, and restore afterwards.
  local netns="" t0 secs rc; t0=$(date +%s)
  [ "$(uname -s)" = "Linux" ] && command -v unshare >/dev/null 2>&1 && netns=1
  if [ -z "$netns" ]; then
    log "  no netns on $(uname -s) — freeing :9741 for the drive"
    if ! stop_resident_daemon; then
      record "6 prod-boot" "SKIP" "$(( $(date +%s)-t0 ))" "could not free :9741 (--no-daemon-manage) — boot chain UNVERIFIED"
      return 0
    fi
  fi
  run_capped "$SMOKE_P6_SECS" scripts/wizard-verify.sh > "$ART/p6-wizard-verify.log" 2>&1
  rc=$?
  if [ -z "$netns" ]; then
    # The drive's EXIT trap kills its own supervised child; give the port a
    # moment to actually release before we start the resident daemon onto it.
    local w=0
    while [ "$w" -lt 15 ] && curl -s --max-time 2 "$DAEMON_URL/v1/models" >/dev/null 2>&1; do
      sleep 1; w=$((w+1))
    done
    restore_resident_daemon
  fi
  secs=$(( $(date +%s) - t0 ))
  case "$rc" in
    0) log "  production boot chain: PASS"
       record "6 prod-boot" "PASS" "$secs" "supervised relaunch verified (branch-2 --daemon-child)" ;;
    # wizard-verify exits 2 for a missing binary/GGUF — a prerequisite gap, not
    # a regression. Same treatment phase 4 gives a missing desktop binary.
    2) warn "  wizard-verify prerequisites missing (see p6-wizard-verify.log)"
       record "6 prod-boot" "SKIP" "$secs" "prerequisite missing (rc=2) — boot chain UNVERIFIED" ;;
    *) err "  production boot chain FAILED (see p6-wizard-verify.log)"
       record "6 prod-boot" "FAIL" "$secs" "wizard-verify rc=$rc" ;;
  esac
}

# ── Phase 5: safety soak (consumes remaining budget) ─────────────────────────
phase5() {
  phase_enabled 5 || { record "5 safety" "SKIP" 0 "disabled"; return 0; }
  local rem; rem=$(remaining)
  # Phase 4 (managed real-mode) runs AFTER us — reserve its allotment
  # (invariant pack + fault suite + margin) so the soak can't starve it.
  if phase_enabled 4 && [ -x "$DESKTOP_BIN" ]; then rem=$(( rem - SMOKE_P4_SECS - 1200 - 120 )); fi
  # Phase 6 also runs after us — reserve it too, or the soak starves the one
  # lane that covers the packaged boot chain.
  if phase_enabled 6 && [ -x "$DESKTOP_BIN" ]; then rem=$(( rem - SMOKE_P6_SECS )); fi
  if [ "$rem" -lt "$SMOKE_P5_MIN_SECS" ]; then record "5 safety" "SKIP" 0 "out of budget (${rem}s left after P4 reserve)"; warn "  budget too tight for soak after reserving Phase 4 — skipping"; return 0; fi
  local mins=$(( rem/60 - 1 )); [ "$mins" -gt 40 ] && mins=40   # cap a smoke soak at 40m
  log "PHASE 5 — safety soak (inner-chaos --minutes $mins)"
  [ -n "$DRY_RUN" ] && { record "5 safety" "DRY" 0 "inner-chaos --minutes <=40"; return 0; }
  local t0; t0=$(date +%s) journal="$ART/p5-inner-chaos.jsonl"
  run_capped $(( (mins+2)*60 )) "$CLI" eval inner-chaos --minutes "$mins" --journal "$journal" \
    > "$ART/p5-safety.log" 2>&1
  local sn; sn=$(grep -oiE 'safety[_ ]number[^0-9]*[0-9.]+' "$ART/p5-safety.log" | grep -oE '[0-9.]+' | tail -1)
  local base="$BASELINE/safety_number.txt" secs=$(( $(date +%s) - t0 ))
  if [ -z "$sn" ]; then record "5 safety" "FAIL" "$secs" "no safety_number emitted"; return 0; fi
  if [ -n "$CAPTURE_BASELINE" ] || [ ! -f "$base" ]; then echo "$sn" > "$base"; record "5 safety" "PASS" "$secs" "captured safety=$sn"; return 0; fi
  local bsn; bsn=$(cat "$base")
  python3 -c "import sys; b,c,d=float('$bsn'),float('$sn'),float('$SAFETY_DROP_ABS'); sys.exit(1 if c < b-d else 0)" \
    && record "5 safety" "PASS" "$secs" "safety=$sn (baseline $bsn)" \
    || record "5 safety" "FAIL" "$secs" "safety $sn dropped >$SAFETY_DROP_ABS vs $bsn"
}

# ── Scoreboard ───────────────────────────────────────────────────────────────
print_scoreboard() {
  echo
  echo "════════════ desktop-smoke scoreboard ($STAMP) ════════════"
  printf '%-18s %-6s %8s   %s\n' "PHASE" "STATUS" "SECS" "DETAIL"
  printf '%-18s %-6s %8s   %s\n' "------------------" "------" "--------" "----------------------------------"
  local p s sec d
  for row in "${ROWS[@]}"; do
    IFS='|' read -r p s sec d <<< "$row"
    printf '%-18s %-6s %8s   %s\n' "$p" "$s" "$sec" "$d"
  done
  echo "─────────────────────────────────────────────────────────"
  echo "total elapsed: $(( $(elapsed)/60 ))m of $(( BUDGET_SECS/60 ))m budget   |   artifacts: $ART"
  { printf '{"stamp":"%s","overall_rc":%s,"elapsed_secs":%s,"rows":[' "$STAMP" "$OVERALL_RC" "$(elapsed)"
    local first=1
    for row in "${ROWS[@]}"; do IFS='|' read -r p s sec d <<< "$row"
      [ $first -eq 1 ] || printf ','; first=0
      printf '{"phase":"%s","status":"%s","secs":%s,"detail":"%s"}' "$p" "$s" "$sec" "${d//\"/}"
    done; printf ']}\n'; } > "$ART/summary.json"
  if [ "$OVERALL_RC" -eq 0 ]; then echo -e "\033[1;32mSMOKE GO — no regression detected in executed phases\033[0m"
  else echo -e "\033[1;31mSMOKE NO-GO — see FAIL rows above\033[0m"; fi
}

# ── Main ─────────────────────────────────────────────────────────────────────
log "desktop-smoke start — budget $(( BUDGET_SECS/60 ))m, primary=$(basename "$TARGET_PRIMARY" .gguf), artifacts $ART${QUICK:+ (quick)}${DRY_RUN:+ (dry-run)}${CAPTURE_BASELINE:+ (capture-baseline)}"
log "host $(uname -s), phase caps via ${TIMEOUT_BIN:-bash fallback (no coreutils timeout)}"
[ -z "$DRY_RUN" ] && basename "$TARGET_PRIMARY" .gguf > "$BASELINE/model.txt" 2>/dev/null || true

if [ -n "$DO_BUILD" ] && [ -z "$DRY_RUN" ]; then
  log "building desktop binary (--build)"; scripts/build-desktop-linux.sh > "$ART/build-desktop.log" 2>&1 || warn "desktop build failed (see build-desktop.log)"
fi

# Phase 0 needs no model. Everything after needs the shipped 35B on :9741.
phase0
if [ -z "$DRY_RUN" ]; then
  if phase_enabled 1 || phase_enabled 2 || phase_enabled 3 || phase_enabled 5; then
    # Distinguish the two failures. rc=2 means the target primary is not on
    # disk, so the bounce was refused and NOTHING was touched — every
    # model-dependent phase after this would measure a machine we declined to
    # configure. That is a precondition failure, not five product failures:
    # HARD STOP with exit 2, which the overnight wrapper records as
    # COULD-NOT-JUDGE rather than FAIL (ARCH_PRINCIPLES §18.1/§18.2 — four
    # verdicts, and "never ran" must never render as "failed").
    ensure_target_primary; etp_rc=$?
    if [ "$etp_rc" -eq 2 ]; then
      err "PHASE PRECONDITION failed — target primary missing; HARD STOP"
      err "  fix \$TARGET_PRIMARY (or pass --primary <path>) and re-run"
      print_scoreboard; exit 2
    fi
    [ "$etp_rc" -ne 0 ] && warn "daemon not on target primary — model-dependent phases may be unrepresentative"
    wait_daemon 30 || warn "daemon not responding on :9741"
  fi
fi
# Order groups by daemon topology: phases 1,2,3,5 share the resident daemon on
# :9741; phase 4 (managed real-mode) owns its OWN hermetic :9741 daemon, so it
# runs after them — it frees the resident daemon and restores it at the end.
# Phase 6 runs in a private netns and touches no shared port, so it goes last
# where a failure cannot cost any other lane its daemon.
phase1
phase2
phase3
phase5
phase4
phase6

print_scoreboard
exit "$OVERALL_RC"
