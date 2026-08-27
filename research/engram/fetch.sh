#!/usr/bin/env bash
# Chunked parallel pull of Qwen3.8-Flash-Next UD-Q4_K_XL.
# Link measured at ~7.6 MB/s ceiling (24 conns); 3 conns only reached 3.5.
# Resumable: each chunk writes at its own offset and drops a .done marker.
set -u
set -o pipefail
B="https://huggingface.co/unsloth/Qwen3.8-Flash-Next-GGUF/resolve/main/UD-Q4_K_XL"
D="/home/alexbryan/dev/commonwealth-ai/sovereign/models/Qwen3.8-Flash-Next"
P="$D/.parts"; mkdir -p "$P"
CHUNK=$((256*1024*1024)); JOBS=24
declare -A SZ=( [1]=10946624 [2]=49859583136 [3]=49376141504 [4]=12087983520 )

get_chunk() {  # $1=shard $2=offset $3=len
  local n=$1 off=$2 len=$3
  local f; f=$(printf 'Qwen3.8-Flash-Next-UD-Q4_K_XL-%05d-of-00004.gguf' "$n")
  local mark="$P/$n.$off.done"
  [ -f "$mark" ] && return 0
  # NEVER `curl | dd && touch`: without pipefail the `&&` tests dd's status, so a
  # short or failed curl still marks the chunk done. On 2026-08-26 that let two
  # chunks land wrong (shard 2 @48855252992, shard 3 @29259464704) and the file
  # still reported ALL CHUNKS SETTLED. Fetch to a temp file, ASSERT the length,
  # then place it.
  local tmp="$P/$n.$off.part"
  rm -f "$tmp"
  curl -sL --fail --retry 8 --retry-all-errors --retry-delay 3 \
       -r "$off-$((off+len-1))" -o "$tmp" "$B/$f" || { rm -f "$tmp"; return 1; }
  local got; got=$(stat -c%s "$tmp" 2>/dev/null || echo 0)
  if [ "$got" -ne "$len" ]; then
    echo "SHORT $n $off got=$got want=$len" >&2; rm -f "$tmp"; return 1
  fi
  dd if="$tmp" of="$D/$f" bs=4M seek="$off" oflag=seek_bytes conv=notrunc status=none \
    && rm -f "$tmp" && touch "$mark"
}
export -f get_chunk; export B D P

for n in 1 2 3 4; do
  f=$(printf 'Qwen3.8-Flash-Next-UD-Q4_K_XL-%05d-of-00004.gguf' "$n")
  [ -f "$D/$f" ] || fallocate -l "${SZ[$n]}" "$D/$f" 2>/dev/null || truncate -s "${SZ[$n]}" "$D/$f"
done

for n in 1 2 3 4; do
  s=${SZ[$n]}; off=0
  while [ "$off" -lt "$s" ]; do
    len=$CHUNK; [ $((off+len)) -gt "$s" ] && len=$((s-off))
    echo "$n $off $len"; off=$((off+len))
  done
done | shuf | xargs -P "$JOBS" -n 3 bash -c 'get_chunk "$0" "$1" "$2"'

echo "ALL CHUNKS SETTLED"
