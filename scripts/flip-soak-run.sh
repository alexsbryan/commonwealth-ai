#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Native-grounding flip soak — the seat-launched run script.
# Order: native-grounding-flip-soak.  Authorizing directive: 7aa64f29.
#
# TWO STAGES IN ONE JOB, and the gate between them is the point:
#
#   STAGE 1  SHAKEDOWN (~6 min, dual)   build + daemon restart + short soak,
#            then `flip-soak-verify.py --mode shakedown`. This validates the
#            instrumentation ADDED on 2026-08-11 — the `grounding` field in
#            both journals, the free-RAM sampler, and the daemonRssMb fix.
#            It says NOTHING about answer quality (ARCH §18.4: validate the
#            instrument before the result).
#
#   STAGE 2  SOAK (120 min, dual)       only if stage 1 passed. Reuses the
#            binaries stage 1 built and the daemon stage 1 restarted, so the
#            seat's restart window is opened exactly ONCE.
#
# If the shakedown fails, stage 2 NEVER STARTS. That is deliberate: two
# hours of dead phases is the documented failure this gate exists to
# prevent, and a failed shakedown is a finding worth reporting on its own.
#
# THE FLAG IS NOT SET ANYWHERE HERE, ON PURPOSE. Since 2026-08-11
# SOVEREIGN_NATIVE_GROUNDING defaults ON, and the whole point of this run
# is to soak THE DEFAULT. Exporting =1 would prove only that an explicit
# opt-in works — which was never in doubt — and would hide a broken
# default. The verifier reports what the turns actually carried.
#
# Usage:  scripts/flip-soak-run.sh [stamp]
# Exit:   0 all good · 1 shakedown failed (no soak run) · 8 memory abort
set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO"

STAMP="${1:-flip-$(date +%Y%m%d-%H%M%S)}"
SHAKE_STAMP="${STAMP}-shakedown"
OUT="$REPO/sovereign/crates/sovereign-desktop/test-artifacts/qa-iterations"
mkdir -p "$OUT"
RUNLOG="$OUT/${STAMP}.runner.log"

# Markers. Every terminal state writes exactly one, so "still running" and
# "died silently" are never confused for each other.
M_RUNNING="$OUT/${STAMP}.RUNNING"
M_SHAKE_FAIL="$OUT/${STAMP}.SHAKEDOWN_FAILED"
M_COMPLETE="$OUT/${STAMP}.RUN_COMPLETE"
M_ABORT="$OUT/${STAMP}.RUN_ABORTED"
rm -f "$M_RUNNING" "$M_SHAKE_FAIL" "$M_COMPLETE" "$M_ABORT"

log() { echo "[$(date +%H:%M:%S)] $*" | tee -a "$RUNLOG"; }

echo "started epoch=$(date +%s) pid=$$ stamp=$STAMP" > "$M_RUNNING"

log "=== flip-soak-run stamp=$STAMP directive=7aa64f29 ==="
log "host: $(hostname) $(uname -sm)"
log "HEAD: $(git rev-parse --short HEAD) on $(git rev-parse --abbrev-ref HEAD)"
# Forensics for the launchd-environment class of failure, which is what
# killed the first launch: launchd handed the job a bare env, so `cargo`
# was not on PATH and `python3` resolved to Xcode's /usr/bin/python3
# (3.9.6) rather than the 3.13 an interactive shell gets. Both python
# scripts are 3.9-parseable and 3.9-tested now, but WHICH interpreter and
# WHICH cargo actually ran is the first thing anyone reading a failed run
# needs, so it is logged rather than reconstructed afterwards.
log "python3: $(command -v python3 || echo '<NOT ON PATH>') — $(python3 -V 2>&1 || true)"
log "cargo:   $(command -v cargo || echo '<NOT ON PATH>')"
log "node:    $(command -v node || echo '<NOT ON PATH>') — $(node -v 2>&1 || true)"
# Refuse EARLY and by name. Without these the run dies minutes later
# inside a build or as a phase that spawns nothing, which reads as a
# product failure rather than a missing PATH entry.
for tool in cargo node python3; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    log "REFUSING: $tool is not on PATH — this is a launcher environment problem, not a soak result"
    echo "$tool missing from PATH epoch=$(date +%s)" > "$M_SHAKE_FAIL"
    rm -f "$M_RUNNING"; exit 1
  fi
done
log "flag: SOVEREIGN_NATIVE_GROUNDING=${SOVEREIGN_NATIVE_GROUNDING:-<unset — the default under test>}"

# ── STAGE 1 ─────────────────────────────────────────────────────────
log "STAGE 1: shakedown (6 min dual) — build + restart + instrument check"
python3 scripts/desktop-soak.py 6 --mode dual --split 0.5 \
  --foreground --stamp "$SHAKE_STAMP" 2>&1 | tee -a "$RUNLOG"
SHAKE_SOAK_RC=${PIPESTATUS[0]}
log "STAGE 1 soak rc=$SHAKE_SOAK_RC"

if [ "$SHAKE_SOAK_RC" -eq 8 ]; then
  log "STAGE 1 ABORTED ON MEMORY — not proceeding to the 2h run"
  echo "shakedown memory abort epoch=$(date +%s)" > "$M_ABORT"
  rm -f "$M_RUNNING"; exit 8
fi

log "STAGE 1: verifying the new instrumentation"
python3 scripts/flip-soak-verify.py --stamp "$SHAKE_STAMP" --mode shakedown 2>&1 | tee -a "$RUNLOG"
VERIFY_RC=${PIPESTATUS[0]}
log "STAGE 1 verify rc=$VERIFY_RC"

# One line the seat asked for: state WHERE the soak's app instance wrote,
# rather than gating on it. --attach copies the resident daemon's own
# config, so the spawned desktop uses the real data dir; both harnesses
# delete the conversations they mint at the end (best-effort).
DATA_DIR=$(python3 - <<'PY'
import os, re
p = os.path.expanduser("~/.sovereign/config.toml")
try:
    m = re.search(r'^\s*dir\s*=\s*"([^"]+)"', open(p).read(), re.M)
    print(m.group(1) if m else "(no [data] dir key — defaults)")
except Exception as e:
    print(f"(unreadable: {e})")
PY
)
log "DATA DIR the attached app wrote to: $DATA_DIR"
log "  (--attach copies the resident daemon config verbatim; harnesses"
log "   delete conversations they minted, best-effort, in their finally block)"

if [ "$VERIFY_RC" -ne 0 ]; then
  log "SHAKEDOWN FAILED — stopping. The 2h run does NOT start."
  echo "shakedown failed rc=$VERIFY_RC epoch=$(date +%s)" > "$M_SHAKE_FAIL"
  rm -f "$M_RUNNING"; exit 1
fi
log "SHAKEDOWN PASSED — proceeding to the 2h soak"

# ── STAGE 2 ─────────────────────────────────────────────────────────
# No rebuild, no second restart: stage 1 already built HEAD and restarted
# the daemon, and re-restarting would reopen the seat's window for nothing.
log "STAGE 2: 120-min dual soak (no rebuild, no second daemon restart)"
python3 scripts/desktop-soak.py 120 --mode dual --split 0.5 \
  --no-build --no-restart --foreground --stamp "$STAMP" 2>&1 | tee -a "$RUNLOG"
SOAK_RC=${PIPESTATUS[0]}
log "STAGE 2 soak rc=$SOAK_RC"

log "STAGE 2: measurement"
python3 scripts/flip-soak-verify.py --stamp "$STAMP" --mode report 2>&1 | tee -a "$RUNLOG"

if [ "$SOAK_RC" -eq 8 ]; then
  log "=== SOAK ABORTED ON MEMORY — report stands as evidence ==="
  echo "soak memory abort epoch=$(date +%s)" > "$M_ABORT"
  rm -f "$M_RUNNING"; exit 8
fi

echo "complete soak_rc=$SOAK_RC epoch=$(date +%s)" > "$M_COMPLETE"
rm -f "$M_RUNNING"
log "=== RUN COMPLETE stamp=$STAMP soak_rc=$SOAK_RC ==="
log "artifacts: $OUT/${STAMP}*"
exit "$SOAK_RC"
