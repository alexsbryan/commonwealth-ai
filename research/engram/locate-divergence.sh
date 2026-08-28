#!/usr/bin/env bash
# Binary-search the first byte where the local chunk stops matching the server.
# Head of each bad chunk matched, tail did not -> expect ONE transition, which is
# what a truncated curl leaves behind.
set -uo pipefail
B="https://huggingface.co/unsloth/Qwen3.8-Flash-Next-GGUF/resolve/main/UD-Q4_K_XL"
D="/home/alexbryan/dev/commonwealth-ai/sovereign/models/Qwen3.8-Flash-Next"
GRAN=4096

match() {  # $1=shard $2=off -> 0 if equal
  local n=$1 off=$2
  local f; f=$(printf 'Qwen3.8-Flash-Next-UD-Q4_K_XL-%05d-of-00004.gguf' "$n")
  local rem loc
  rem=$(curl -sL --retry 6 --retry-all-errors --retry-delay 2 \
        -r "$off-$((off+GRAN-1))" "$B/$f" | sha256sum | cut -d' ' -f1)
  loc=$(dd if="$D/$f" skip="$off" count="$GRAN" iflag=skip_bytes,count_bytes \
        status=none | sha256sum | cut -d' ' -f1)
  [ "$rem" = "$loc" ]
}

bisect() {  # $1=shard $2=chunk-start $3=chunk-end
  local n=$1 lo=$2 hi=$3 mid
  echo "shard $n chunk [$lo .. $hi)" >&2
  match "$n" "$lo" || { echo "  head ALSO differs -- assumption broken" >&2; return 1; }
  while [ $((hi - lo)) -gt "$GRAN" ]; do
    mid=$(( (lo + hi) / 2 )); mid=$(( mid - mid % GRAN ))
    if match "$n" "$mid"; then lo=$mid; else hi=$mid; fi
    printf '\r  window %14d bytes' $((hi-lo)) >&2
  done
  echo >&2
  echo "$n $hi"
}

bisect 2 48855252992 49123688448
bisect 3 29259464704 29527900160
