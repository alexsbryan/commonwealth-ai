#!/usr/bin/env bash
# Score one LoRA adapter: fuse -> gguf q8_0 -> llama-server -> eval -> curve.
#
#   ./scripts/score_checkpoint.sh <name> <adapter-dir>
#
# e.g. ./scripts/score_checkpoint.sh rung-2928 runs/m3-4b-ab/hf/checkpoint-2928
#
# Each stage is skipped if its output already exists, so a re-run after a
# failure resumes rather than restarts.
#
# WHY THIS WAS REWRITTEN (2026-08-05, note 6d18a622). The previous version was
# zsh, Mac-only, and pinned to the 0.8B: a hardcoded `SNAP=…Qwen3.5-0.8B/
# snapshots/2fc0636…`, `.venv`/`.venv-bespoke` that exist on no other box, and
# `~/dev/llama.cpp` assumed present. None of the M3 ladder could have been
# scored with it. Every path is now RESOLVED and PRINTED rather than assumed —
# a scoring run that cannot say which base model it fused against is not a
# measurement.
#
# Everything is overridable by environment variable; the defaults are what the
# Halo has. Overrides that matter:
#   BASE        base model dir (default: resolved from the adapter's config)
#   PY          the eval/fuse interpreter    LLAMA_SERVER  the server binary
#   CONVERT_PY  interpreter for the GGUF converter (see the note below)
#   PER_SUBSET  rows per subset (default 200 -> the ~2,186-item card)
#   CTX         PER-SLOT context (default 32768; the KV pool is CTX x CONCURRENCY)
#   THRESHOLD   p_grounded decision threshold; adds the third scoring column
#   PORT        default 8089
#
# ON THE HALO, RUN IT AS:
#   toolbox run -c sovereign-vulkan env PER_SUBSET=50 ./scripts/score_checkpoint.sh …
#
# THE `env` IS LOAD-BEARING AND ITS ABSENCE IS SILENT. `toolbox run` does NOT
# inherit the calling shell's environment — measured 2026-08-05:
#   FOO=bar toolbox run -c sovereign-vulkan bash -c 'echo $FOO'  -> empty
#   toolbox run -c sovereign-vulkan env FOO=bar bash -c 'echo $FOO' -> bar
# So the documented `PER_SUBSET=50 toolbox run … score_checkpoint.sh` form drops
# EVERY knob above and silently runs the defaults. Caught it live scoring rung 1:
# the banner said `per-subset 200`, i.e. a 6-hour full card where a 90-minute
# rung was asked for. The banner is what caught it, which is why every resolved
# value is printed — a scoring run that cannot say what it scored is not a
# measurement (§18.3: never silently substitute).
set -uo pipefail

cd "$(dirname "$0")/.."   # research/verifier-v0

[ $# -eq 2 ] || { echo "usage: $0 <name> <adapter-dir>" >&2; exit 64; }
NAME=$1
ADAPTER=$2

die() { echo "FATAL: $*" >&2; exit "${2:-2}"; }

# -- resolve the interpreter -------------------------------------------------
PY=${PY:-}
if [ -z "$PY" ]; then
  for c in "$HOME/dev/train-env/.venv/bin/python" .venv/bin/python python3; do
    command -v "$c" >/dev/null 2>&1 && { PY=$c; break; }
  done
fi
[ -n "$PY" ] || die "no python interpreter found; set PY="

# THE transformers-4.x SPLIT IS MAC-ONLY AND WAS MEASURED FALSE HERE. The old
# script insisted conversion needed 4.49.0 because 5.14.1 hashes the tokenizer
# differently and misses the qwen35 pre-tokenizer entry. On this box, under
# transformers 5.14.1 and llama.cpp b10236, the 4B converted cleanly (441
# tensors). Default CONVERT_PY to the same interpreter; override it only if you
# actually hit "BPE pre-tokenizer was not recognized".
CONVERT_PY=${CONVERT_PY:-$PY}
CONVERT=${CONVERT:-$HOME/dev/llama.cpp/convert_hf_to_gguf.py}
[ -f "$CONVERT" ] || die "no convert_hf_to_gguf.py at $CONVERT (set CONVERT=)"
export PYTHONPATH="$(dirname "$CONVERT")/gguf-py${PYTHONPATH:+:$PYTHONPATH}"

# -- resolve the server ------------------------------------------------------
# /usr/bin/llama-server ON THIS BOX IS b6153 (Jan 2026) AND CANNOT LOAD qwen35:
#   "error loading model architecture: unknown model architecture: 'qwen35'"
# The CONVERTER writes qwen35 happily, so conversion succeeds and only serving
# fails — the worst possible ordering, and it cost a debugging round. Prefer the
# locally built server; if we fall back to one on PATH, say so out loud.
LLAMA_SERVER=${LLAMA_SERVER:-}
if [ -z "$LLAMA_SERVER" ]; then
  for c in "$HOME/dev/llama.cpp/build/bin/llama-server" \
           "$HOME/dev/llama.cpp/build/bin/server"; do
    [ -x "$c" ] && { LLAMA_SERVER=$c; break; }
  done
fi
if [ -z "$LLAMA_SERVER" ]; then
  LLAMA_SERVER=$(command -v llama-server 2>/dev/null || true)
  [ -n "$LLAMA_SERVER" ] && echo "WARNING: falling back to $LLAMA_SERVER — if it cannot load qwen35, build llama.cpp: cmake -B build -DGGML_VULKAN=ON -DCMAKE_BUILD_TYPE=Release -DLLAMA_CURL=OFF && cmake --build build -j --target llama-server" >&2
fi
[ -n "$LLAMA_SERVER" ] || die "no llama-server found; set LLAMA_SERVER="

# -- resolve the base model --------------------------------------------------
# From the adapter's own config, so a 0.8B adapter and a 4B adapter each fuse
# against the right base without an edit. `base_model_name_or_path` records the
# path on the machine that TRAINED it (a pod), which does not exist here — so
# match on basename across the local model roots and die naming what was tried.
if [ -z "${BASE:-}" ]; then
  want=$("$PY" - "$ADAPTER" <<'PYEOF' 2>/dev/null
import json, os, sys
try:
    cfg = json.load(open(os.path.join(sys.argv[1], "adapter_config.json")))
except OSError:
    sys.exit(1)
print(os.path.basename(str(cfg.get("base_model_name_or_path", "")).rstrip("/")))
PYEOF
)
  [ -n "$want" ] || die "cannot read base_model_name_or_path from $ADAPTER/adapter_config.json"
  tried=""
  for root in "${TRAIN_ENV:-$HOME/dev/train-env}/models" "$HOME/dev/train-env/models"; do
    tried="$tried
  $root/$want"
    [ -d "$root/$want" ] && { BASE="$root/$want"; break; }
  done
  if [ -z "${BASE:-}" ]; then
    hub="$HOME/.cache/huggingface/hub/models--Qwen--$want/snapshots"
    tried="$tried
  $hub/*"
    snap=$(ls -d "$hub"/*/ 2>/dev/null | head -1)
    [ -n "$snap" ] && BASE="${snap%/}"
  fi
  [ -n "${BASE:-}" ] || die "base model '$want' not found. Tried:$tried"
fi
[ -d "$BASE" ] || die "BASE=$BASE is not a directory"

PORT=${PORT:-8089}
CTX=${CTX:-32768}
PER_SUBSET=${PER_SUBSET:-200}
CONCURRENCY=${CONCURRENCY:-4}
LOGPROBS=${LOGPROBS:-10}
SOURCE=${SOURCE:-data/llm-aggrefact/test.parquet}
OUT=${OUT:-runs/scored/$NAME}
GGUF=$OUT/$NAME-q8.gguf

mkdir -p "$OUT"
LOG=$OUT/score.log
say() { echo "$(date +%FT%T) $*" | tee -a "$LOG"; }

# EVERY RESOLVED PATH, ONCE, BEFORE ANY WORK. A card that cannot name the base
# it fused against, the server that served it and the bank it scored is not a
# measurement you can compare to another one.
say "=== $NAME ==="
say "  adapter   $ADAPTER"
say "  base      $BASE"
say "  py        $PY"
say "  convert   $CONVERT (under $CONVERT_PY)"
say "  server    $LLAMA_SERVER"
say "  source    $SOURCE  (per-subset $PER_SUBSET, ctx $CTX, logprobs $LOGPROBS)"
[ -n "${THRESHOLD:-}" ] && say "  threshold $THRESHOLD"

[ -f "$ADAPTER/adapter_model.safetensors" ] || [ -f "$ADAPTER/adapters.safetensors" ] \
  || die "no adapter_model.safetensors (PEFT) or adapters.safetensors (mlx) in $ADAPTER"
if command -v lsof >/dev/null 2>&1 && lsof -ti :$PORT >/dev/null 2>&1; then
  die "port $PORT busy"
fi

# -- fuse + convert ----------------------------------------------------------
if [ ! -f "$GGUF" ]; then
  # mlx_lm's own fuse corrupts Qwen3.5 (drops mtp.*, mishandles the hybrid
  # linear_attn/self_attn merge) — M0_PROBE.md. Use the manual fuse, which
  # auto-detects PEFT vs mlx layout and asserts tensor parity.
  say "fusing"
  "$PY" scripts/fuse_lora_manual.py --snapshot "$BASE" --adapter "$ADAPTER" \
      --out "$OUT/fused" > "$OUT/fuse.log" 2>&1 \
    || { tail -5 "$OUT/fuse.log" >&2; die "FUSE FAILED — see $OUT/fuse.log" 4; }
  say "  $(grep -E '^fused ' "$OUT/fuse.log" | tail -1)"

  say "convert_hf_to_gguf q8_0"
  "$CONVERT_PY" "$CONVERT" "$OUT/fused" --outfile "$GGUF" --outtype q8_0 \
      > "$OUT/convert.log" 2>&1 \
    || { tail -5 "$OUT/convert.log" >&2; die "CONVERT FAILED — see $OUT/convert.log" 5; }
  # The fused fp16 dir is ~9GB and is reproducible from base+adapter in 12s.
  # Keeping it per rung would be ~100GB across a 12-rung ladder.
  [ "${KEEP_FUSED:-0}" = "1" ] || rm -rf "$OUT/fused"
else
  say "gguf present — skipping fuse/convert"
fi

# -- serve + eval ------------------------------------------------------------
if [ -f "$OUT/eval/summary.json" ]; then
  say "summary present — skipping eval"
else
  # `-c` IS THE TOTAL KV POOL AND llama-server DIVIDES IT BY --parallel, so the
  # limit an individual request meets is CTX/CONCURRENCY, not CTX. Passing
  # `-c 32768 --parallel 4` gave every slot 8192 — and the failure is a per-item
  # HTTP 400 that the eval records as an error and drops, so the card comes back
  # SHORT rather than wrong, which is the harder thing to notice (§18.3).
  #
  # MEASURED on the full 29,320-row bank (doc+claim, chars/3.5):
  #   p50 ~600 tok · p99 ~5,062 · max ~34,408
  #   over  8,192 tok: 136 rows (0.46%)   <- predicted the 3/550 seen at rung 1
  #   over 32,768 tok:   4 rows (0.014%)  <- the residual, named not hidden
  # So CTX now means PER-SLOT context — the number a caller actually reasons
  # about ("does my longest item fit?") — and the pool is derived from it.
  KV_TOTAL=$(( CTX * CONCURRENCY ))
  say "serving $GGUF"
  say "  ctx $CTX/slot x $CONCURRENCY slots = $KV_TOTAL total KV"
  "$LLAMA_SERVER" -m "$GGUF" --port "$PORT" -c "$KV_TOTAL" --parallel "$CONCURRENCY" \
      -ngl 99 --no-warmup > "$OUT/server.log" 2>&1 &
  SPID=$!
  OK=0
  for _ in $(seq 1 120); do
    curl -sf "http://127.0.0.1:$PORT/v1/models" >/dev/null 2>&1 && { OK=1; break; }
    kill -0 "$SPID" 2>/dev/null || break
    sleep 5
  done
  if [ "$OK" -ne 1 ]; then
    kill "$SPID" 2>/dev/null
    grep -q "unknown model architecture" "$OUT/server.log" 2>/dev/null && \
      echo "  -> this server is too old for this architecture; build llama.cpp and set LLAMA_SERVER=" >&2
    tail -5 "$OUT/server.log" >&2
    die "server never came up — see $OUT/server.log" 3
  fi

  say "scoring (${PER_SUBSET}/subset)"
  mkdir -p "$OUT/eval"
  "$PY" scripts/eval_grounding.py \
    --run-dir "$OUT/eval" \
    --source "$SOURCE" \
    --base-url "http://127.0.0.1:$PORT/v1" \
    --per-subset "$PER_SUBSET" --seed 17 --concurrency "$CONCURRENCY" \
    --no-think --grammar --logprobs "$LOGPROBS" \
    ${THRESHOLD:+--decision-threshold "$THRESHOLD"} \
    >> "$OUT/eval.log" 2>&1
  RC=$?
  kill "$SPID" 2>/dev/null; wait "$SPID" 2>/dev/null
  say "eval exit=$RC"
  [ "$RC" -eq 0 ] || exit "$RC"
fi

# -- report ------------------------------------------------------------------
# THREE NUMBERS, NOT ONE (M3_RUN_OF_SHOW §4). BAcc is the leaderboard metric,
# AUC is the one M3 has to move, and tnr at a fixed false-alarm budget is the
# PRODUCT metric — a rung can gain BAcc purely by sliding the threshold, and
# only AUC tells the two apart. The curve's tpr 90 / tpr 95 rows ARE the 10%
# and 5% false-alarm budgets.
"$PY" - "$OUT/eval/summary.json" <<'PYEOF' 2>&1 | tee -a "$LOG"
import json, sys
d = json.load(open(sys.argv[1]))
p = d.get("parse", {})
line = (f"tolerant BAcc {d['macro_avg_bacc_tolerant']:.2f}   "
        f"strict {d['macro_avg_bacc']:.2f}")
if d.get("macro_avg_bacc_threshold") is not None:
    line += f"   threshold {d['macro_avg_bacc_threshold']:.2f}"
print(line)
print(f"  scored {p.get('scored')}  unparseable {p.get('failures_tolerant')}  "
      f"errors {d.get('errors')}")
if d.get("errors"):
    print("  WARNING: non-zero errors — those rows are MISSING from the card, "
          "not scored wrong. Raise CTX and re-run before quoting this.")
print(f"\n{'subset':<20}{'BAcc':>8}{'tpr_sup':>10}{'tnr_hall':>10}")
for k, v in sorted(d['subsets_tolerant'].items()):
    print(f"{k:<20}{v['bacc']:>8.2f}{v['tpr_supported']:>10.2f}{v['tnr_hallucinated']:>10.2f}")
PYEOF

say "--- operating curve (AUC + tnr at each false-alarm budget) ---"
"$PY" scripts/operating_curve.py "$OUT/eval" 2>&1 | tee -a "$LOG"

say "done: $OUT"
