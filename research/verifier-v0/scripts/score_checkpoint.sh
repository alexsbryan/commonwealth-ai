#!/bin/zsh
# Score one LoRA adapter on the full LLM-AggreFact card.
#
#   ./scripts/score_checkpoint.sh <name> <adapter-dir>
#
# e.g. ./scripts/score_checkpoint.sh A150 runs/mix-study/A/adapters
#
# fuse -> gguf q8_0 -> llama-server -> eval_grounding.py --no-think.
# Outputs land in runs/mix-study/<name>/. Each step is skipped if its output
# already exists, so a re-run after a failure resumes rather than restarts.
set -u

cd "$(dirname "$0")/.."   # research/verifier-v0

[ $# -eq 2 ] || { echo "usage: $0 <name> <adapter-dir>" >&2; exit 64; }
NAME=$1
ADAPTER=$2

SNAP=$HOME/.cache/huggingface/hub/models--Qwen--Qwen3.5-0.8B/snapshots/2fc06364715b967f1860aea9cf38778875588b17
CONVERT=$HOME/dev/llama.cpp/convert_hf_to_gguf.py
PY=.venv/bin/python

# convert_hf_to_gguf.py must run under transformers 4.x, NOT the training venv.
# It identifies the tokenizer by hashing the token ids of a fixed check string;
# transformers 5.14.1 (in .venv) encodes it differently, so the hash misses the
# qwen35 entry and conversion dies with "BPE pre-tokenizer was not recognized".
# Measured 2026-08-02: .venv -> 1444df51..., .venv-bespoke (4.49.0) ->
# d30d75d9... which is the qwen35 hash the converter expects. gguf comes from
# llama.cpp's own gguf-py so it is version-matched to the converter.
CONVERT_PY=.venv-bespoke/bin/python
export PYTHONPATH=$HOME/dev/llama.cpp/gguf-py${PYTHONPATH:+:$PYTHONPATH}
PORT=${PORT:-8089}
OUT=runs/mix-study/$NAME
GGUF=$OUT/$NAME-q8.gguf

mkdir -p "$OUT"
LOG=$OUT/score.log
say() { echo "$(date +%FT%T) $*" | tee -a "$LOG"; }

[ -f "$ADAPTER/adapters.safetensors" ] || { say "no adapters.safetensors in $ADAPTER"; exit 2; }
lsof -ti :$PORT >/dev/null 2>&1 && { say "port $PORT busy"; exit 2; }

if [ ! -f "$GGUF" ]; then
  # mlx_lm's own fuse corrupts Qwen3.5 (drops mtp.*, mishandles the hybrid
  # linear_attn/self_attn merge) — M0_PROBE.md. Use the manual fuse.
  say "fusing $ADAPTER"
  $PY scripts/fuse_lora_manual.py --snapshot "$SNAP" --adapter "$ADAPTER" \
      --out "$OUT/fused" > "$OUT/fuse.log" 2>&1 \
    || { say "FUSE FAILED — see $OUT/fuse.log"; exit 4; }

  say "convert_hf_to_gguf q8_0 (under $CONVERT_PY)"
  $CONVERT_PY "$CONVERT" "$OUT/fused" --outfile "$GGUF" --outtype q8_0 \
      > "$OUT/convert.log" 2>&1 \
    || { say "CONVERT FAILED — see $OUT/convert.log"; exit 5; }
else
  say "gguf present — skipping fuse/convert"
fi

if [ -f "$OUT/eval/summary.json" ]; then
  say "summary present — skipping eval"
else
  say "serving $GGUF"
  llama-server -m "$GGUF" --port $PORT -c 32768 --parallel 4 \
      -ngl 99 --no-warmup > "$OUT/server.log" 2>&1 &
  SPID=$!
  OK=0
  for i in {1..120}; do
    curl -sf "http://127.0.0.1:$PORT/v1/models" >/dev/null 2>&1 && { OK=1; break; }
    kill -0 "$SPID" 2>/dev/null || break
    sleep 5
  done
  [ "$OK" -eq 1 ] || { say "server never came up — see $OUT/server.log"; kill "$SPID" 2>/dev/null; exit 3; }

  say "scoring full 2,200-item card (--no-think)"
  mkdir -p "$OUT/eval"
  $PY scripts/eval_grounding.py \
    --run-dir "$OUT/eval" \
    --source data/llm-aggrefact/test.parquet \
    --base-url "http://127.0.0.1:$PORT/v1" \
    --per-subset 200 --seed 17 --concurrency 4 --no-think \
    >> "$OUT/eval.log" 2>&1
  RC=$?
  kill "$SPID" 2>/dev/null; wait "$SPID" 2>/dev/null
  say "eval exit=$RC"
  [ "$RC" -eq 0 ] || exit "$RC"
fi

say "=== $NAME ==="
$PY - "$OUT/eval/summary.json" <<'PYEOF' 2>&1 | tee -a "$LOG"
import json, sys
d = json.load(open(sys.argv[1]))
print(f"tolerant BAcc {d['macro_avg_bacc_tolerant']:.2f}   "
      f"strict {d['macro_avg_bacc']:.2f}   "
      f"rescued {d['parse']['rescued_by_tolerant']}  "
      f"unparseable {d['parse']['failures_tolerant']}  "
      f"scored {d['parse']['scored']}")
print(f"\n{'subset':<20}{'BAcc':>8}{'tpr_sup':>10}{'tnr_hall':>10}")
for k, v in sorted(d['subsets_tolerant'].items()):
    print(f"{k:<20}{v['bacc']:>8.2f}{v['tpr_supported']:>10.2f}{v['tnr_hallucinated']:>10.2f}")
PYEOF
