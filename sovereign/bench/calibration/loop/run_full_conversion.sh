#!/usr/bin/env bash
# Order competence-conversion-loop — the full-bank arbitration runs.
#
# TWO serial full-bank runs (saltgrass, 42 probes each) under PRODUCTION
# DEFAULTS (no SOVEREIGN_RERANK_MODEL_PATH, no SOVEREIGN_NATIVE_GROUNDING)
# with the fixed binary (worktree HEAD: prompt-admission budgets + the
# citation matched-chunk widening). Two runs because the done-when is a
# rate on a bank with two known run-to-run flippers — one run is not a
# measurement (ARCH §18.5).
#
# The done-when bars (order, verbatim): bench red-line
# competence-when-present >= 0.74 (23/31), honesty-when-absent >= 0.91,
# production defaults, dev bank only. The frozen holdout is NOT touched.
#
# Markers: "=== FULL<n> START/END exit=<rc> ===" per arm, "=== CONVERSION
# RUNS COMPLETE ===" terminal. A non-zero bench exit is a GATE verdict,
# not a harness failure — rows written is the success criterion.
set -uo pipefail
cd "$(dirname "$0")/../../../.."   # repo root (the -fix worktree)

BENCH=sovereign/bench/chaos_monkey
OUT=sovereign/bench/calibration/loop
CLI=${CLI:-target/debug/sovereign-cli-llm}

[ -x "$CLI" ] || { echo "no binary at $CLI" >&2; exit 2; }
curl -sf --max-time 10 http://localhost:9741/v1/models >/dev/null \
  || { echo "REFUSING: daemon not answering on :9741" >&2; exit 4; }

unset SOVEREIGN_RERANK_MODEL_PATH SOVEREIGN_NATIVE_GROUNDING
export RUST_LOG=info,sovereign_core::runtime::grounding=debug,sovereign::retrieval=debug

for N in 1 2; do
  echo "=== FULL${N} START $(date -Iseconds) ==="
  "$CLI" bench chaos-monkey run \
    --bank "$BENCH/saltgrass.toml" \
    --manifest "$BENCH/manifest.toml" \
    --out "$OUT/full_after_r${N}.jsonl" \
    --transcripts "$OUT/full_after_r${N}.transcripts.jsonl" \
    > "$OUT/full_after_r${N}.run.log" 2>&1
  rc=$?
  echo "=== FULL${N} END $(date -Iseconds) exit=$rc ==="
done
echo "=== CONVERSION RUNS COMPLETE $(date -Iseconds) ==="
