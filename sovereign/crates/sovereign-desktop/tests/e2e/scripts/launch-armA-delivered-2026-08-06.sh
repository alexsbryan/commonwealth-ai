#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# ARM A — the paired control run for the soak-quality arc (2026-08-06).
#
# WHAT THIS IS FOR, in one line: establish the reference every later
# "did the fix help?" comparison is measured against, on a tree whose
# journal finally reports what the model actually read.
#
# Why a control arm is not optional. Arm B cannot be compared against the
# 2026-08-06 generative baseline: that run had a different question stream
# (chaos invents its questions), a different sampling temperature, and a
# journal whose evidence counts described the pre-eviction pool. Comparing
# across those differences produces a confident number from unpaired data —
# the exact failure the SOVEREIGN_CHAOS_REPLAY guard in chaos.mjs now
# refuses to allow silently.
#
# WHAT EACH PHASE BUYS, because they answer different questions:
#
#   chaos (replay, ~58 min) — PAIRED. Drives the 110-question stratified
#     bank (59 baseline-BROKEN + 51 baseline-good, interleaved so a
#     truncated run still samples both strata). Temp 0. This is the arm a
#     later fix is McNemar-tested against via paired-ab.mjs.
#
#   personas (generative, ~32 min) — NOT PAIRED, and not pretending to be.
#     personas.mjs has no replay mode. Its value here is the decline
#     taxonomy under the new `evidencePresenceDelivered` pair, which is the
#     only surface that can split a synthesis failure from a truncation
#     failure. The 2026-08-06 breakdown cannot: its presence judge read the
#     full resolved pool while the model read at most 600 chars per chunk.
#
# --no-build: the tree is already built (workspace lint clean, sovereign-core
#   873 lib tests + 6 splice tests green). Rebuilding here would only risk
#   picking up a peer session's in-flight edits mid-run.
# --no-restart: the streaming runtime lives in the DESKTOP process, not the
#   daemon, so a fresh desktop binary is sufficient — and restarting the
#   daemon would evict the resident model out from under the other sessions
#   sharing this box.
#
# Both arms MUST run against the same daemon and the same desktop binary,
# differing only in the fix under test. Do not rebuild between them.
set -euo pipefail

REPO=/home/alexbryan/dev/commonwealth-ai
BANK="$REPO/sovereign/crates/sovereign-desktop/test-artifacts/qa-iterations/synthfix-stratified.bank.jsonl"
STAMP="${STAMP:-armA-delivered-2026-08-06}"
MINUTES="${MINUTES:-90}"

[ -s "$BANK" ] || { echo "FATAL: replay bank missing or empty: $BANK" >&2; exit 2; }
echo "arm A: $(wc -l < "$BANK") paired questions, ${MINUTES} min, stamp=${STAMP}"

# Temp 0 for the paired comparison: sampling variance is the dominant noise
# source in a 110-question arm, and §18.5 asks the delta to survive noise at
# the sample size used. It makes the ABSOLUTE numbers differ from the
# 2026-08-06 baseline — that is expected and is why arm A exists at all.
export SOVEREIGN_CHAOS_REPLAY="$BANK"
export SOVEREIGN_SYNTH_TEMP=0

exec "$REPO/scripts/desktop-soak.py" "$MINUTES" \
  --mode dual --split 0.65 \
  --no-build --no-restart \
  --stamp "$STAMP"
