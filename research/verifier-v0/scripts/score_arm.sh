#!/usr/bin/env bash
# Score one checkpoint on the full 2,200-item llm-aggrefact card.
# The scoring half of the Mac's scripts/run_mix_study.sh, ported to the Halo.
#
#   NAME=base                       ./score_arm.sh    # control, no adapter
#   NAME=mix-A ADAPTER=runs/mix-A/adapter ./score_arm.sh
#
# WHY --lora AND NOT FUSE. The Mac fused because `mlx_lm fuse` was its only
# option — and that path corrupts Qwen3.5 (drops mtp.*, mishandles the hybrid
# linear_attn/self_attn merge). llama-server applies the adapter to the SAME
# base GGUF the control is scored on, so an arm and the control differ by the
# adapter ALONE: no re-quantisation, no merged-model conversion.
#
# WHY WE RE-SCORE THE BASE HERE. The 54.2 control was measured on the Mac. Any
# cross-box comparison silently folds in quantisation, sampler and template
# differences. Scoring the base through THIS stack costs ~30 min and removes
# every one of those confounds. Report against the local control; keep 54.2 as
# a sanity check, not as the denominator.
set -u
cd /home/alexbryan/dev/commonwealth-ai/research/verifier-v0

NAME=${NAME:?set NAME}
ADAPTER=${ADAPTER:-}
PORT=${PORT:-8089}
SEED=${SEED:-17}
PER_SUBSET=${PER_SUBSET:-200}
# The benchmark rows. Defaults to LLM-AggreFact so every historical invocation
# means what it did before. FaithBench is the OTHER half of the v0 card and the
# one the adopt candidate collapses on (49.57 strict, TNR 16.17 -- BASELINES.md),
# so it needs to be as easy to run as the headline bench:
#   SOURCE=data/faithbench/test.jsonl PER_SUBSET=750 ...
# Named in summary.json via the eval's own `source` field, so a run can never be
# mistaken for one against a different card.
SOURCE=${SOURCE:-data/llm-aggrefact/test.parquet}
# GRAMMAR=1 constrains decoding to the <answer> schema (eval_grounding
# ANSWER_GBNF). It is a PROTOCOL CHANGE: a grammar run and a free-decode run are
# not comparable, because the grammar removes parse failures that the free run
# scores as wrong answers. Score every arm in a comparison the same way, control
# included, and say which protocol the number came from.
GRAMMAR=${GRAMMAR:-0}
# CONCURRENCY is part of the PROTOCOL, not a speed knob. Proven 2026-08-03: the
# same item, same adapter, same flags gets a different verdict depending on what
# else is in flight when it decodes (note 255a1819). Changing it changes the
# number. The historical baselines were all measured at 4.
CONCURRENCY=${CONCURRENCY:-4}

TE=/home/alexbryan/dev/train-env
PY=$TE/.venv/bin/python
LLAMA=/home/alexbryan/dev/llama.cpp
BASE_HF=$TE/models/Qwen3.5-0.8B
# q8_0 to match the quantisation the Mac's control was measured under.
BASE_GGUF=$TE/gguf/Qwen3.5-0.8B-Q8_0.gguf
OUT=$TE/runs/score-$NAME
mkdir -p "$OUT" "$TE/gguf"

export PYTHONPATH=$LLAMA/gguf-py${PYTHONPATH:+:$PYTHONPATH}

say() { echo "$(date +%FT%T) [$NAME] $*" | tee -a "$OUT/orchestrator.log"; }

# ------------------------------------------------------------- preflight
# Prove every leg BEFORE serving. A missing piece discovered after a 30-minute
# scoring run is the same waste the GGUF trap caused after a night of training.
for f in "$BASE_HF/config.json" "$LLAMA/convert_hf_to_gguf.py" \
         "$LLAMA/convert_lora_to_gguf.py" data/llm-aggrefact/test.parquet \
         scripts/eval_grounding.py "$PY"; do
  [ -e "$f" ] || { say "PREFLIGHT FAIL: missing $f"; exit 2; }
done
command -v llama-server >/dev/null || { say "PREFLIGHT FAIL: no llama-server"; exit 2; }
if command -v ss >/dev/null && ss -ltn 2>/dev/null | grep -q ":$PORT "; then
  say "PREFLIGHT FAIL: port $PORT busy"; exit 2
fi
[ -z "$ADAPTER" ] || [ -f "$ADAPTER/adapter_model.safetensors" ] \
  || { say "PREFLIGHT FAIL: no adapter_model.safetensors under $ADAPTER"; exit 2; }

# An adapter that never trained scores as the base model. Refuse to spend 30
# minutes discovering that (HALO_HANDOFF_2026-08-02.md §4).
if [ -n "$ADAPTER" ]; then
  $PY scripts/check_adapter_trained.py "$ADAPTER" >>"$OUT/orchestrator.log" 2>&1 \
    || { say "PREFLIGHT FAIL: adapter did not pass the gate — scoring it would"
         say "  produce a base-model number under an arm's name."; exit 2; }
fi

# --------------------------------------------------------------- convert
if [ ! -f "$BASE_GGUF" ]; then
  say "converting base -> q8_0"
  $PY "$LLAMA/convert_hf_to_gguf.py" "$BASE_HF" --outfile "$BASE_GGUF" \
      --outtype q8_0 >"$OUT/convert-base.log" 2>&1 \
    || { say "BASE CONVERT FAILED — see $OUT/convert-base.log"; exit 4; }
fi

LORA_ARGS=()
if [ -n "$ADAPTER" ]; then
  AG=$TE/gguf/$NAME-adapter-F16.gguf
  if [ ! -f "$AG" ]; then
    say "converting adapter -> gguf"
    $PY "$LLAMA/convert_lora_to_gguf.py" "$ADAPTER" --base "$BASE_HF" \
        --outfile "$AG" --outtype f16 >"$OUT/convert-lora.log" 2>&1 \
      || { say "LORA CONVERT FAILED — see $OUT/convert-lora.log"; exit 5; }
  fi
  LORA_ARGS=(--lora "$AG")
fi

# ----------------------------------------------------------------- serve
say "serving $(basename "$BASE_GGUF") ${ADAPTER:+with adapter $(basename "$AG")}"
llama-server -m "$BASE_GGUF" "${LORA_ARGS[@]}" \
  --port "$PORT" --host 127.0.0.1 -c 32768 --parallel 4 -ngl 99 --no-warmup \
  >"$OUT/server.log" 2>&1 &
SPID=$!
trap 'kill $SPID 2>/dev/null' EXIT

ok=0
for _ in $(seq 1 120); do
  curl -sf "http://127.0.0.1:$PORT/v1/models" >/dev/null 2>&1 && { ok=1; break; }
  kill -0 "$SPID" 2>/dev/null || break
  sleep 5
done
[ "$ok" -eq 1 ] || { say "server never came up — see $OUT/server.log"; exit 3; }

# Prove the adapter is actually applied, not silently ignored. A GGUF that
# loads without its LoRA scores as the base and looks like a null result.
if [ -n "$ADAPTER" ]; then
  n=$(curl -s "http://127.0.0.1:$PORT/lora-adapters" | grep -c '"scale"' || true)
  [ "$n" -ge 1 ] || { say "ADAPTER NOT LOADED by llama-server — refusing to score"; exit 6; }
  say "adapter confirmed loaded: $(curl -s http://127.0.0.1:$PORT/lora-adapters)"
fi

# ----------------------------------------------------------------- score
# --no-think is MANDATORY for the 0.8B: with thinking on it burns the token cap
# in reasoning_content and emits zero verdicts.
EVAL_ARGS=()
[ "$GRAMMAR" = "1" ] && EVAL_ARGS+=(--grammar)
# LOGPROBS=N records p_grounded per item -- the model's probability of GROUNDED
# at the token the grammar makes decisive. Off by default because it changes the
# response payload and every committed baseline was measured without it. With
# it, one run yields the whole tpr/tnr curve instead of the single point a hard
# label gives, which is what separates "this arm discriminates better" from
# "this arm sits at a friendlier threshold". The verdict columns are unaffected,
# so a LOGPROBS run is still protocol-comparable on bacc.
[ -n "${LOGPROBS:-}" ] && EVAL_ARGS+=(--logprobs "$LOGPROBS")

say "scoring $SOURCE (--no-think, --per-subset $PER_SUBSET, seed $SEED, grammar=$GRAMMAR, concurrency=$CONCURRENCY, logprobs=${LOGPROBS:-0})"
$PY scripts/eval_grounding.py \
  --run-dir "$OUT" \
  --source "$SOURCE" \
  --base-url "http://127.0.0.1:$PORT/v1" \
  --per-subset "$PER_SUBSET" \
  --seed "$SEED" \
  --concurrency "$CONCURRENCY" \
  --no-think \
  "${EVAL_ARGS[@]}" \
  >>"$OUT/eval.log" 2>&1
RC=$?
kill "$SPID" 2>/dev/null; wait "$SPID" 2>/dev/null
say "eval exit=$RC — artifacts in $OUT"
exit $RC
