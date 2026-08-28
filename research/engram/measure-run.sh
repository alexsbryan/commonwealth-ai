#!/usr/bin/env bash
# Run the target config and MEASURE the three things the objective names:
#   1. tok/s                     (llama-completion's own timings)
#   2. mem_info_gtt_used         (is the engram actually off the GPU?)
#   3. major-fault rate          (is it being demand-paged, or just page-cached?)
# Sampled, not assumed -- an unwatched gate is not a gate.
set -uo pipefail
R="/home/alexbryan/dev/commonwealth-ai"
BIN="$R/target/llama-cmake-cache/91029babe06584d2/bin/llama-completion"
M="$R/sovereign/models/Qwen3.8-Flash-Next/Qwen3.8-Flash-Next-UD-Q4_K_XL-00001-of-00004.gguf"
OUT="$R/research/engram"
TAG="${TAG:-run}"
OT="${OT-per_layer_token_embd=CPU}"
NGL="${NGL:-99}"
NPRED="${NPRED:-128}"

: > "$OUT/$TAG.samples.tsv"
echo -e "t_s\tgtt_used_gib\tmajflt\tminflt\trss_gib" >> "$OUT/$TAG.samples.tsv"

sample() {
  local pid=$1 t0 now maj min rss gtt
  t0=$(date +%s.%N)
  while kill -0 "$pid" 2>/dev/null; do
    gtt=$(cat /sys/class/drm/card*/device/mem_info_gtt_used 2>/dev/null | head -1)
    read -r _ _ _ _ _ _ _ _ _ min _ maj _ < /proc/"$pid"/stat 2>/dev/null || break
    rss=$(awk '/^VmRSS/{print $2}' /proc/"$pid"/status 2>/dev/null)
    now=$(date +%s.%N)
    awk -v t="$now" -v t0="$t0" -v g="${gtt:-0}" -v mj="${maj:-0}" -v mn="${min:-0}" -v r="${rss:-0}" \
      'BEGIN{printf "%.1f\t%.2f\t%s\t%s\t%.2f\n", t-t0, g/1073741824, mj, mn, r/1048576}' \
      >> "$OUT/$TAG.samples.tsv"
    sleep 1
  done
}

echo "=== config: -ngl $NGL  -n $NPRED  override=$([ -n "$OT" ] && echo "-ot '$OT'" || echo NONE) ==="
"$BIN" -m "$M" -ngl "$NGL" ${OT:+-ot "$OT"} ${EXTRA:-} -c 4096 -n "$NPRED" -s 42 -no-cnv --no-warmup \
  -p "Explain, in three sentences, why memory bandwidth rather than compute is the binding constraint for single-stream LLM inference." \
  > "$OUT/$TAG.log" 2>&1 &
LP=$!
sample "$LP" &
SP=$!
wait "$LP"; rc=$?
kill "$SP" 2>/dev/null
echo "llama exit rc=$rc"
echo "--- peak gtt / final faults ---"
awk 'NR>1{if($2>g)g=$2; mj=$3; r=$5} END{printf "peak_gtt=%.2f GiB  final_majflt=%s  peak_rss=%.2f GiB\n", g, mj, r}' \
  "$OUT/$TAG.samples.tsv"
