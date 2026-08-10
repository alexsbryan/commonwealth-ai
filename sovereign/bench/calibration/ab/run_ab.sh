#!/usr/bin/env bash
# D5 — the native-grounding A/B: flag-off vs flag-on, SERIAL, one arm at a time.
#
# WHY BOTH ARMS CARRY THE RERANKER. `SOVEREIGN_RERANK_MODEL_PATH` is set in
# BOTH arms and only `SOVEREIGN_NATIVE_GROUNDING` differs. With the reranker
# in the flag-on arm alone this would confound two changes: H1's admission
# decision AND the reranker's effect on RETRIEVAL, because
# `search_with_rerank` changes which chunks survive. Holding it constant
# isolates the flag.
#
# READ THE CONTROL CORRECTLY. The flag-off arm is therefore NOT today's
# production default — this host has no rerank slot configured. It is the
# correct control for the flag, not a picture of production.
#
# VALIDATE THE INSTRUMENT BEFORE THE RESULT. `SOVEREIGN_RERANK_MODEL_PATH`
# is unset by default here. Without it H1 returns `NoInstrument` on every
# turn, flag-on becomes byte-identical to flag-off, and the A/B reads as a
# clean no-regression while measuring nothing. A 2-probe smoke confirmed the
# reranker loads and H1 actually fires (margin_source=retrieval_rerank_score,
# pool=8) before the hours were committed. The guard below refuses rather
# than repeating that risk silently.
#
# WHICH MODEL. qwen3-reranker-0.6b-q8_0 is the SAME reranker the H1 kill gate
# scored on. The committed thresholds live in that model's logit scale, so a
# different reranker would silently invalidate them.
#
# BANK. saltgrass only — the dev bank carrying both classes the HARD bars
# need (20 `present`, 11 absent). `saltgrass_compound` has ZERO absent
# probes, so its honesty gate is a 0/0 NaN and it cannot speak to bar (a);
# it is named as not-run in the verdict rather than silently dropped.
set -uo pipefail
cd "$(dirname "$0")/../../../.."   # repo root

BENCH=sovereign/bench/chaos_monkey
OUT=sovereign/bench/calibration/ab
CLI=${CLI:-target/debug/sovereign-cli-llm}
RERANK=${SOVEREIGN_RERANK_MODEL_PATH:-/Users/alexsbryan/.cache/huggingface/hub/models--ggml-org--Qwen3-Reranker-0.6B-Q8_0-GGUF/snapshots/a02f48bb4f057028298c21fa033da2b30d7742d5/qwen3-reranker-0.6b-q8_0.gguf}

[ -x "$CLI" ] || { echo "no binary at $CLI — cargo build -p sovereign-cli-llm" >&2; exit 2; }
[ -f "$RERANK" ] || { echo "REFUSING: no reranker at $RERANK — H1 would report NoInstrument on every turn and the A/B would be void" >&2; exit 3; }
# The daemon has NO /healthz (404); /v1/models is the liveness surface.
curl -sf --max-time 10 http://localhost:9741/v1/models >/dev/null \
  || { echo "REFUSING: daemon not answering on :9741" >&2; exit 4; }

mkdir -p "$OUT"
export SOVEREIGN_RERANK_MODEL_PATH="$RERANK"
export RUST_LOG=info,sovereign_core::runtime::grounding=debug

for ARM in off on; do
  echo "=== ARM=$ARM START $(date -Iseconds) ==="
  if [ "$ARM" = "on" ]; then export SOVEREIGN_NATIVE_GROUNDING=1; else unset SOVEREIGN_NATIVE_GROUNDING; fi
  "$CLI" bench chaos-monkey run \
    --bank "$BENCH/saltgrass.toml" \
    --manifest "$BENCH/manifest.toml" \
    --out "$OUT/ab_saltgrass_${ARM}.jsonl" \
    --transcripts "$OUT/ab_saltgrass_${ARM}.transcripts.jsonl" \
    > "$OUT/ab_saltgrass_${ARM}.run.log" 2>&1
  rc=$?
  # A non-zero exit is a BENCH GATE verdict (competence/honesty), not a
  # harness failure. The run's success is judged by whether the rows were
  # written, which ab_verdict.py then reads.
  echo "=== ARM=$ARM END $(date -Iseconds) exit=$rc ==="
done
echo "=== AB COMPLETE $(date -Iseconds) ==="
echo "now: python3 $OUT/ab_verdict.py $OUT $OUT/ab_verdict.json"
