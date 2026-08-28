#!/usr/bin/env bash
# How does this hybrid GDN/QSA model's memory scale with context?
# KV (attention layers) vs RS (recurrent state) vs compute buffer.
# The production daemon runs context_size = 65536, so that is the number
# that decides whether Flash-Next is a viable primary slot.
set -uo pipefail
R=/home/alexbryan/dev/commonwealth-ai
BIN="$R/target/llama-cmake-cache/91029babe06584d2/bin/llama-completion"
M="$R/sovereign/models/Qwen3.8-Flash-Next/Qwen3.8-Flash-Next-UD-Q4_K_XL-00001-of-00004.gguf"
printf '%-9s %-12s %-12s %-14s %-10s\n' ctx KV_MiB RS_MiB compute_MiB gtt_GiB
for ctx in 4096 16384 32768 65536; do
  log="$R/research/engram/kv-$ctx.log"
  "$BIN" -m "$M" -ngl 99 -c "$ctx" -n 1 --no-warmup -no-cnv -v -p "hi" > "$log" 2>&1
  kv=$(grep -aoE 'KV buffer size = *[0-9.]+' "$log" | grep -oE '[0-9.]+' | awk '{s+=$1} END{printf "%.1f", s}')
  rs=$(grep -aoE 'RS buffer size = *[0-9.]+' "$log" | grep -oE '[0-9.]+' | awk '{s+=$1} END{printf "%.1f", s}')
  cb=$(grep -aoE 'compute buffer size = *[0-9.]+' "$log" | grep -oE '[0-9.]+' | awk '{if($1>m)m=$1} END{printf "%.1f", m}')
  gtt=$(awk -v b="$(cat /sys/class/drm/card*/device/mem_info_gtt_used|head -1)" 'BEGIN{printf "%.1f", b/1073741824}')
  printf '%-9s %-12s %-12s %-14s %-10s\n' "$ctx" "${kv:-?}" "${rs:-?}" "${cb:-?}" "$gtt"
done
