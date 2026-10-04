#!/usr/bin/env bash
# Middle-loop dev-bank A/B — OFF ARM ONLY, order native-grounding-tuning-loop
# (directive 44f48dd6). See manifest.md beside this file.
#
# Validates the two LANDED component wins on branch
# native-grounding-tuning-loop against the plan's pre-registered bars:
#   honesty-when-absent  = 1.00 (11/11; css-center converts via the
#                          structural GK caveat, plan §4.2 [A5])
#   competence-when-present >= 0.74 (23/31, the committed off-arm level —
#                          non-regression under the routing exemplars +
#                          deep-spawn prefix emission)
#
# One arm: the flag stays OFF on this branch; flag-on parity is the parity
# plan's own work. Mirrors ab/run_ab.sh's off-arm leg exactly (incl. the
# reranker held constant) but writes to target/loop-ab/ so the COMMITTED
# ab_saltgrass_off.* evidence is never clobbered.
set -uo pipefail
cd "$(dirname "$0")/../.."   # repo root

BENCH=sovereign/bench/chaos_monkey
OUT=target/loop-ab
CLI=${CLI:-target/debug/sovereign-cli-llm}
RERANK=${SOVEREIGN_RERANK_MODEL_PATH:-/Users/alexsbryan/.cache/huggingface/hub/models--ggml-org--Qwen3-Reranker-0.6B-Q8_0-GGUF/snapshots/a02f48bb4f057028298c21fa033da2b30d7742d5/qwen3-reranker-0.6b-q8_0.gguf}

[ -x "$CLI" ] || { echo "no binary at $CLI — cargo build -p sovereign-cli-llm" >&2; exit 2; }
[ -f "$RERANK" ] || { echo "REFUSING: no reranker at $RERANK — instrument would differ from the committed A/B" >&2; exit 3; }
curl -sf --max-time 10 http://localhost:9741/v1/models >/dev/null \
  || { echo "REFUSING: daemon not answering on :9741" >&2; exit 4; }

mkdir -p "$OUT"
export SOVEREIGN_RERANK_MODEL_PATH="$RERANK"
export RUST_LOG=info,sovereign_core::runtime::grounding=debug
unset SOVEREIGN_NATIVE_GROUNDING

echo "=== ARM=off (loop re-run) START $(date -Iseconds) branch=$(git branch --show-current) head=$(git rev-parse --short HEAD) ==="
"$CLI" bench chaos-monkey run \
  --bank "$BENCH/saltgrass.toml" \
  --manifest "$BENCH/manifest.toml" \
  --out "$OUT/loop_saltgrass_off.jsonl" \
  --transcripts "$OUT/loop_saltgrass_off.transcripts.jsonl" \
  > "$OUT/loop_saltgrass_off.run.log" 2>&1
rc=$?
echo "=== ARM=off END $(date -Iseconds) exit=$rc ==="
grep -E 'RED-LINE|VERDICT' "$OUT/loop_saltgrass_off.run.log" | tail -8
echo "rows: $(wc -l < "$OUT/loop_saltgrass_off.jsonl" 2>/dev/null || echo 0)"
