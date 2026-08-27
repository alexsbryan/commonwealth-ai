#!/usr/bin/env bash
# Re-fetch the two damaged 256 MiB chunks.
#
# Fixes the bug in fetch.sh that let this land silently: `curl | dd && touch $mark`
# tests the PIPELINE status, which without pipefail is dd's -- so a short or wrong
# curl body still marked its chunk done. Here every piece goes to its own file,
# its length is ASSERTED against the requested length, and only then is it placed.
set -euo pipefail
B="https://huggingface.co/unsloth/Qwen3.8-Flash-Next-GGUF/resolve/main/UD-Q4_K_XL"
D="/home/alexbryan/dev/commonwealth-ai/sovereign/models/Qwen3.8-Flash-Next"
T="$D/.repair"; mkdir -p "$T"
PIECE=$((32*1024*1024))

fetch_piece() {  # $1=shard $2=offset $3=len
  local n=$1 off=$2 len=$3
  local f; f=$(printf 'Qwen3.8-Flash-Next-UD-Q4_K_XL-%05d-of-00004.gguf' "$n")
  local tmp="$T/$n.$off.bin"
  rm -f "$tmp"
  curl -sL --fail --retry 8 --retry-all-errors --retry-delay 3 \
       -r "$off-$((off+len-1))" -o "$tmp" "$B/$f"
  local got; got=$(stat -c%s "$tmp")
  if [ "$got" -ne "$len" ]; then
    echo "SHORT $n $off got=$got want=$len" >&2; rm -f "$tmp"; return 1
  fi
  echo "GOT   $n $off $len"
}
export -f fetch_piece; export B D T

emit() {  # $1=shard $2=chunk_start $3=chunk_len
  local n=$1 cs=$2 cl=$3
  local off=$cs
  local end=$((cs+cl))
  local len
  while [ "$off" -lt "$end" ]; do
    len=$PIECE; [ $((off+len)) -gt "$end" ] && len=$((end-off))
    echo "$n $off $len"; off=$((off+len))
  done
}

{ emit 2 48855252992 268435456; emit 3 29259464704 268435456; } \
  | xargs -r -P 8 -n 3 bash -c 'fetch_piece "$0" "$1" "$2"'

echo "--- all pieces fetched and length-checked; placing ---"
for tmp in "$T"/*.bin; do
  base=$(basename "$tmp" .bin); n=${base%%.*}; off=${base#*.}
  f=$(printf 'Qwen3.8-Flash-Next-UD-Q4_K_XL-%05d-of-00004.gguf' "$n")
  dd if="$tmp" of="$D/$f" bs=4M seek="$off" oflag=seek_bytes conv=notrunc status=none
  echo "PLACED $n $off $(stat -c%s "$tmp")"
done
sync
echo "REPAIR COMPLETE"
