#!/usr/bin/env bash
# P1's desktop rendering witness, BOTH ARMS — order native-grounding-p1-desktop,
# deliverable (2). Drives `tests/e2e/real/native-grounding-p1.real.spec.ts`
# twice against the REAL app: once flag-off, once flag-on. Artifacts land in
# test-artifacts/p1-desktop-{off,on}*.{json,png}.
#
# WHY NOT scripts/desktop-soak.py. The canonical soak (chaos + personas) is an
# ANSWER-QUALITY harness: it drives the real app against the RESIDENT daemon and
# judges what the model said. It asserts nothing about the DOM, captures no
# screenshot, and — decisively — cannot change the flag, because the flag is read
# in the daemon process (admission.rs:153) and the soak attaches to the operator's
# daemon rather than owning one. Turning the flag on there means restarting the
# operator's daemon (a seat-owned seam) and still leaves the rendering unwitnessed.
# The real-mode e2e harness (playwright.real.config.ts + tests/e2e/real/
# global-setup.ts) already spawns its OWN fixture-scoped daemon with our env
# inherited, already plants a sealed fixture corpus, and already drives the real
# Svelte surface. Reused whole; this script only chooses the arms (ARCH §19).
#
# WHAT IT NEEDS
#   :9741 free      — managed mode starts its own daemon there and REFUSES if the
#                     port is taken (global-setup.ts). Stop the resident daemon
#                     first; this script does not touch it.
#   a reranker      — H1 has no instrument without one, `answer_segments` is never
#                     computed, and the flag-on arm would film a flag-off screen.
#                     Set in BOTH arms so the only difference between them is the
#                     flag (same reasoning as bench/calibration/ab/run_ab.sh).
#   cargo           — global-setup builds sovereign-desktop + sovereign-cli-daemon.
#
# Usage:  tests/e2e/scripts/p1-desktop-render.sh [off|on|both]      (default both)
set -uo pipefail
ARMS=${1:-both}
E2E_DIR="$(cd "$(dirname "$0")/.." && pwd)"
CRATE_ROOT="$(cd "$E2E_DIR/../.." && pwd)"
REPO_ROOT="$(cd "$CRATE_ROOT/../../.." && pwd)"
ART="$CRATE_ROOT/test-artifacts"
SPEC=tests/e2e/real/native-grounding-p1.real.spec.ts
RERANK=${SOVEREIGN_RERANK_MODEL_PATH:-/Users/alexsbryan/.cache/huggingface/hub/models--ggml-org--Qwen3-Reranker-0.6B-Q8_0-GGUF/snapshots/a02f48bb4f057028298c21fa033da2b30d7742d5/qwen3-reranker-0.6b-q8_0.gguf}

mkdir -p "$ART"
rm -f "$ART/p1-desktop.DONE"

[ -f "$RERANK" ] || {
  echo "REFUSING: no reranker at $RERANK — H1 would report NoInstrument on every turn and the flag-on arm would be void" >&2
  exit 3
}
if nc -z 127.0.0.1 9741 2>/dev/null; then
  echo "REFUSING: something is listening on :9741. Real-mode managed daemon needs the port free." >&2
  echo "          Stop the resident daemon (seat-owned) and re-run." >&2
  exit 4
fi

export SOVEREIGN_RERANK_MODEL_PATH="$RERANK"
# The daemon global-setup spawns inherits this; grounding at debug so the
# admission + segmentation events are in test-artifacts/real-daemon.log.
export RUST_LOG=${RUST_LOG:-info,sovereign_core::runtime::grounding=debug}

rc_off=0
rc_on=0
run_arm() {
  local arm=$1
  echo "=== ARM=$arm START $(date -Iseconds) ==="
  # BOTH arms state the flag explicitly. Since the 2026-08-11 promotion
  # the default is ON, so `unset` would silently run the on-path and this
  # script would film two on-arms while labelling one of them "off".
  if [ "$arm" = "on" ]; then export SOVEREIGN_NATIVE_GROUNDING=1; else export SOVEREIGN_NATIVE_GROUNDING=0; fi
  ( cd "$CRATE_ROOT" && npx playwright test -c playwright.real.config.ts "$SPEC" ) \
    > "$ART/p1-desktop-$arm.run.log" 2>&1
  local rc=$?
  # Keep the scratch profile for any following arm: the fixture + governance
  # ingests are the slow part of setup and the corpora are identical across arms.
  export SOVEREIGN_REAL_KEEP_PROFILE=1
  echo "=== ARM=$arm END $(date -Iseconds) exit=$rc ==="
  return $rc
}

case "$ARMS" in
  off)  run_arm off; rc_off=$? ;;
  on)   run_arm on;  rc_on=$? ;;
  both) run_arm off; rc_off=$?; run_arm on; rc_on=$? ;;
  *) echo "usage: $0 [off|on|both]" >&2; exit 2 ;;
esac

echo "off_rc=$rc_off on_rc=$rc_on epoch=$(date +%s)" > "$ART/p1-desktop.DONE"
echo "=== P1 DESKTOP RENDER COMPLETE off_rc=$rc_off on_rc=$rc_on ==="
echo "artifacts: $ART/p1-desktop-*.json  $ART/p1-desktop-*.png  $ART/p1-desktop-*.run.log"
[ "$rc_off" -eq 0 ] && [ "$rc_on" -eq 0 ]
