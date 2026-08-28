#!/usr/bin/env bash
# Locate corruption at the granularity of the FETCH UNIT, not at random.
# fetch.sh pulled 256 MiB chunks aligned at 0; a wholly-wrong chunk is therefore
# caught with certainty by probing its head and tail. (The earlier 47 random
# 16 KiB probes covered 0.0015% of the file and proved nothing -- LAB-RECORD.)
set -uo pipefail
B="https://huggingface.co/unsloth/Qwen3.8-Flash-Next-GGUF/resolve/main/UD-Q4_K_XL"
D="/home/alexbryan/dev/commonwealth-ai/sovereign/models/Qwen3.8-Flash-Next"
CHUNK=$((256*1024*1024)); PROBE=$((32*1024))
declare -A SZ=( [2]=49859583136 [3]=49376141504 )

probe() {  # $1=shard $2=offset  -- offset is a byte offset, count is PROBE bytes
  local n=$1 off=$2
  local f; f=$(printf 'Qwen3.8-Flash-Next-UD-Q4_K_XL-%05d-of-00004.gguf' "$n")
  local end=$((off + PROBE - 1))
  local rem loc
  rem=$(curl -sL --retry 6 --retry-all-errors --retry-delay 2 -r "$off-$end" "$B/$f" \
        | sha256sum | cut -d' ' -f1)
  loc=$(dd if="$D/$f" skip="$off" count="$PROBE" iflag=skip_bytes,count_bytes \
        status=none | sha256sum | cut -d' ' -f1)
  if [ -z "$rem" ] || [ "$rem" = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855" ]; then
    echo "ERR  $n $off empty-remote"
  elif [ "$rem" = "$loc" ]; then
    echo "OK   $n $off"
  else
    echo "DIFF $n $off"
  fi
}
export -f probe; export B D PROBE

for n in 2 3; do
  s=${SZ[$n]}; off=0
  while [ "$off" -lt "$s" ]; do
    ce=$((off + CHUNK)); [ "$ce" -gt "$s" ] && ce=$s
    echo "$n $off"                       # head of chunk
    echo "$n $((ce - PROBE))"            # tail of chunk
    off=$ce
  done
done | xargs -P 16 -n 2 bash -c 'probe "$0" "$1"'
echo "PROBE COMPLETE"
