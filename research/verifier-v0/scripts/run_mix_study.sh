#!/bin/zsh
# M2 mix study — does Stream B help? Two matched-iteration 0.8B ORPO arms.
#
#   arm A    data/orpo-76k   (Stream A only,      74,674 pairs)
#   arm AB   data/orpo-ab    (A + B, B share .199, 93,693 pairs)
#
# Matched ITERS, not matched epochs: at eff-batch 32 both arms see exactly the
# same number of examples, so the only thing that differs is the mixture.
# (M2_MAC_MIGRATION_OUTCOME.md §6.)
#
# ITERS=400 is leg 1 of a resumable staircase, not a truncated full run. See
# findings/M2_MIX_STUDY_DESIGN.md for why 400 and how to extend to 800 without
# invalidating the comparison.
#
# Stage 0 scores the EXISTING 100-iter probe on the same full 2,200-item card
# under the same protocol. Without that reference line a null result at 400
# ("both arms equal") is uninterpretable — it cannot be told apart from "the
# recipe learns nothing past iter 100". 27 minutes of insurance on a 12h bet.
#
# Serial by construction. Peak trainer RSS is 27 of 64 GB; two arms at once is
# what locked the machine on 2026-07-29. The 40 GB tripwire is inherited from
# runs/probe-0.8b-orpo/relaunch.sh.
set -u

cd "$(dirname "$0")/.."   # research/verifier-v0

ITERS=${ITERS:-400}
SEED=${SEED:-17}
ROOT=runs/mix-study
SNAP=$HOME/.cache/huggingface/hub/models--Qwen--Qwen3.5-0.8B/snapshots/2fc06364715b967f1860aea9cf38778875588b17
CONVERT=$HOME/dev/llama.cpp/convert_hf_to_gguf.py
PY=.venv/bin/python

# convert_hf_to_gguf.py must run under transformers 4.x, NOT the training venv.
# It hashes the token ids of a fixed check string to identify the tokenizer;
# transformers 5.14.1 (in .venv) encodes it differently, so the hash misses the
# qwen35 entry and conversion dies with "BPE pre-tokenizer was not recognized".
CONVERT_PY=.venv-bespoke/bin/python
export PYTHONPATH=$HOME/dev/llama.cpp/gguf-py${PYTHONPATH:+:$PYTHONPATH}
RSS_KILL_KB=$((40 * 1024 * 1024))
PORT=8089

mkdir -p "$ROOT"
MAIN="$ROOT/orchestrator.log"
say() { echo "$(date +%FT%T) $*" | tee -a "$MAIN"; }

# ---------------------------------------------------------------- preflight
for f in "$SNAP/config.json" "$CONVERT" "$PY" data/orpo-76k/train.jsonl \
         data/orpo-ab/train.jsonl data/llm-aggrefact/test.parquet \
         scripts/fuse_lora_manual.py scripts/eval_grounding.py; do
  [ -e "$f" ] || { say "PREFLIGHT FAIL: missing $f"; exit 2; }
done
command -v llama-server >/dev/null || { say "PREFLIGHT FAIL: no llama-server"; exit 2; }
lsof -ti :$PORT >/dev/null 2>&1 && { say "PREFLIGHT FAIL: port $PORT busy"; exit 2; }

# Prove the GGUF leg BEFORE training for hours. On 2026-08-02 a transformers
# upgrade in .venv silently broke conversion; the failure surfaces only after
# the arm has already trained, which is the whole night wasted.
$CONVERT_PY -c "import gguf, transformers, torch" 2>/dev/null \
  || { say "PREFLIGHT FAIL: $CONVERT_PY cannot import gguf/transformers/torch"; exit 2; }
CHK=$($CONVERT_PY -c "
import hashlib, re, sys
src = open('$CONVERT').read()
chktxt = eval(re.search(r'chktxt = (.*?)\n\s*chktok', src, re.S).group(1).strip())
from transformers import AutoTokenizer
tok = AutoTokenizer.from_pretrained('$SNAP')
print(hashlib.sha256(str(tok.encode(chktxt)).encode()).hexdigest())
" 2>/dev/null)
grep -q "$CHK" "$CONVERT" 2>/dev/null \
  || { say "PREFLIGHT FAIL: tokenizer chkhsh $CHK not in $CONVERT — conversion would die AFTER training"; exit 2; }

say "preflight ok — iters=$ITERS seed=$SEED, gguf leg verified (chkhsh ${CHK:0:12})"

# ------------------------------------------------------------------- train
train_arm() {
  local name=$1 data=$2
  local run="$ROOT/$name"
  if [ -f "$run/adapters/adapters.safetensors" ]; then
    say "[$name] adapters present — skipping train"; return 0
  fi
  mkdir -p "$run"
  cat > "$run/config-lora-only.yaml" <<'YAML'
# lora_parameters has no CLI flag; everything else is passed on the CLI because
# train.py's YAML merge fills only args whose value is None.
lora_parameters:
  rank: 32
  dropout: 0.0
  scale: 2.0
YAML

  say "[$name] train start — $data, $ITERS iters"
  $PY -m mlx_lm_lora.train \
    --model Qwen/Qwen3.5-0.8B \
    --train \
    --train-mode orpo \
    --train-type lora \
    --optimizer adamw \
    --data "$data" \
    --seed "$SEED" \
    --batch-size 4 \
    --gradient-accumulation-steps 8 \
    --iters "$ITERS" \
    --learning-rate 1e-4 \
    --beta 0.1 \
    --max-seq-length 4096 \
    --num-layers -1 \
    --steps-per-report 10 \
    --steps-per-eval 50 \
    --val-batches 10 \
    --save-every 50 \
    --adapter-path "$run/adapters" \
    -c "$run/config-lora-only.yaml" \
    > "$run/train.log" 2>&1 &
  local tpid=$!
  echo "$(date +%T) launched trainer pid=$tpid" > "$run/mem.log"

  while kill -0 "$tpid" 2>/dev/null; do
    local rss_kb=$(ps -o rss= -p "$tpid" | tr -d ' ')
    local free_gb=$(vm_stat | awk '/Pages free/{gsub("\\.","",$3); printf "%.1f", $3*16384/1073741824}')
    echo "$(date +%T) rss=$((${rss_kb:-0} / 1048576))GB free=${free_gb}GB" >> "$run/mem.log"
    if [ "${rss_kb:-0}" -gt "$RSS_KILL_KB" ]; then
      echo "$(date +%T) RSS TRIPWIRE >40GB — killing trainer" >> "$run/mem.log"
      say "[$name] RSS TRIPWIRE — killed"
      kill "$tpid"
    fi
    sleep 30
  done
  wait "$tpid"; local rc=$?
  say "[$name] train exit=$rc"
  [ "$rc" -eq 0 ] || return "$rc"
}

# ------------------------------------------------- fuse -> gguf -> score
score_model() {
  local name=$1 gguf=$2 outdir=$3
  if [ -f "$outdir/summary.json" ]; then
    say "[$name] summary present — skipping score"; return 0
  fi
  mkdir -p "$outdir"
  say "[$name] serving $gguf"
  llama-server -m "$gguf" --port $PORT -c 32768 --parallel 4 \
      -ngl 99 --no-warmup > "$ROOT/$name.server.log" 2>&1 &
  local spid=$!
  local ok=0
  for i in {1..120}; do
    curl -sf "http://127.0.0.1:$PORT/v1/models" >/dev/null 2>&1 && { ok=1; break; }
    kill -0 "$spid" 2>/dev/null || break
    sleep 5
  done
  [ "$ok" -eq 1 ] || { say "[$name] server never came up"; kill "$spid" 2>/dev/null; return 3; }

  say "[$name] scoring full 2,200-item card (--no-think)"
  $PY scripts/eval_grounding.py \
    --run-dir "$outdir" \
    --source data/llm-aggrefact/test.parquet \
    --base-url "http://127.0.0.1:$PORT/v1" \
    --per-subset 200 \
    --seed "$SEED" \
    --concurrency 4 \
    --no-think \
    >> "$ROOT/$name.eval.log" 2>&1
  local rc=$?
  kill "$spid" 2>/dev/null; wait "$spid" 2>/dev/null
  say "[$name] eval exit=$rc"
  return "$rc"
}

build_gguf() {
  local name=$1
  local run="$ROOT/$name"
  local gguf="$run/$name-q8.gguf"
  # every diagnostic here goes to stderr — stdout is the captured gguf path
  if [ -f "$gguf" ]; then say "[$name] gguf present" >&2; echo "$gguf"; return 0; fi

  # mlx_lm's own fuse silently corrupts Qwen3.5 (drops mtp.*, mishandles the
  # hybrid linear_attn/self_attn merge) — M0_PROBE.md §. Use the manual fuse.
  say "[$name] fusing adapter into HF snapshot" >&2
  $PY scripts/fuse_lora_manual.py \
      --snapshot "$SNAP" --adapter "$run/adapters" --out "$run/fused" \
      > "$run/fuse.log" 2>&1 || { say "[$name] FUSE FAILED" >&2; return 4; }

  say "[$name] convert_hf_to_gguf q8_0 (under $CONVERT_PY)" >&2
  $CONVERT_PY "$CONVERT" "$run/fused" --outfile "$gguf" --outtype q8_0 \
      > "$run/convert.log" 2>&1 || { say "[$name] CONVERT FAILED" >&2; return 5; }
  echo "$gguf"
}

# ------------------------------------------------------------------ stages
say "=== stage 0: reference line — existing 100-iter probe, full card ==="
score_model "ref-probe100" runs/probe-0.8b-orpo/probe-orpo-0.8b-q8.gguf \
            "$ROOT/ref-probe100/eval" \
  || say "WARNING: reference score failed — arms will still run"

for arm in "A:data/orpo-76k" "AB:data/orpo-ab"; do
  name=${arm%%:*}; data=${arm#*:}
  say "=== arm $name ($data) ==="
  train_arm "$name" "$data" || { say "arm $name TRAIN FAILED — stopping"; exit 1; }
  gguf=$(build_gguf "$name") || { say "arm $name BUILD FAILED — stopping"; exit 1; }
  score_model "$name" "$gguf" "$ROOT/$name/eval" \
    || say "WARNING: arm $name score failed"
done

# ----------------------------------------------------------------- verdict
say "=== results ==="
$PY scripts/summarize_mix_study.py 2>&1 | tee -a "$MAIN"

say "=== done ==="
