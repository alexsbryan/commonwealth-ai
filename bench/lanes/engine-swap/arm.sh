#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Drive PREREG_ENGINE_SWAP_20261009.md from the HOST (it calls into the
# sovereign-vulkan toolbox itself).
#
#   arm.sh up L|R       restart the resident daemon on arm L or R
#   arm.sh check <run>  the eight `check` lanes, one invocation each, with
#                       each lane's wall window recorded for census.py
#   arm.sh restore      put the resident config back and restart on it
#
# ENGINE_SWAP_ENV names a file of KEY=VALUE lines: the resident daemon's
# own environment, captured from /proc/<pid>/environ. Both arms start with it,
# so they differ only in [engine]. It holds secrets and stays outside the repo.
#
# R is the same daemon with [engine] kind = "remote". It reaches
# engine_proxy.py on :18300, which forwards to one llama-server router on
# :18301, built by build-llama-server.sh --llguidance.
set -eu
repo=$(cd "$(dirname "$0")/../../.." && pwd)
out=$repo/target/engine-swap
cfg=$HOME/.sovereign/config.toml
models=$repo/sovereign/models
embed=$repo/models/qwen-embedding-0.6b.gguf/Qwen3-Embedding-0.6B-Q8_0.gguf
server=$repo/target/llama-server-vanilla/build-llg/bin/llama-server
lanes="chat-ask throughput routing retrieval-prod enrichment-f1 chaos-monkey knowledge-gym synth"
mkdir -p "$out"
tb() { toolbox run -c sovereign-vulkan "$@"; }

daemon_down() {
  tb sovereign daemon stop >/dev/null 2>&1 || true
  i=0; while curl -sf -m 2 localhost:9741/health >/dev/null; do
    i=$((i + 1)); [ $i -lt 60 ] || { echo "arm: daemon still answering after 60s" >&2; exit 1; }; sleep 1
  done
}

daemon_up() {
  : "${ENGINE_SWAP_ENV:?names the resident daemon env file}"
  tb env $(cat "$ENGINE_SWAP_ENV") sovereign daemon start >"$out/daemon-start-$1.log" 2>&1
  curl -sf -m 5 localhost:9741/health >/dev/null || { echo "arm: daemon not up, see $out/daemon-start-$1.log" >&2; exit 1; }
  echo "arm: daemon up on arm $1, pid $(pgrep -x sovereign-stock), exe $(readlink -f /proc/$(pgrep -x sovereign-stock)/exe)"
}

router_down() {
  for f in "$out/proxy.pid" "$out/router.pid"; do
    [ -f "$f" ] && kill "$(cat "$f")" 2>/dev/null; rm -f "$f"
  done
}

router_up() {
  cat > "$out/router.ini" <<EOF
version = 1

[*]
n-gpu-layers = 99

[Qwen3.6-35B-A3B-MTP-UD-Q6_K]
model = $models/Qwen3.6-35B-A3B-MTP-UD-Q6_K.gguf
ctx-size = 65536
spec-type = draft-mtp
spec-draft-n-max = 3

[Qwen3.5-4B-UD-MTP-Q6_K_XL]
model = $models/Qwen3.5-4B-UD-MTP-Q6_K_XL.gguf
ctx-size = 65536
spec-type = draft-mtp
spec-draft-n-max = 3

[Qwen3-Embedding-0.6B-Q8_0]
model = $embed
embedding = true
ctx-size = 8192
batch-size = 8192
ubatch-size = 8192
EOF
  tb sh -c "'$server' --models-preset '$out/router.ini' --models-max 4 --host 127.0.0.1 --port 18301 \
    > '$out/router.log' 2>&1 & echo \$! > '$out/router.pid'"
  i=0; until curl -sf localhost:18301/v1/models >/dev/null; do
    i=$((i + 1)); [ $i -lt 120 ] || { echo "arm: router not up, see $out/router.log" >&2; exit 1; }; sleep 1
  done
  python3 "$repo/bench/lanes/engine-swap/engine_proxy.py" --listen 127.0.0.1:18300 \
    --upstream http://127.0.0.1:18301 --log "$out/proxy.jsonl" --arm R \
    > "$out/proxy.log" 2>&1 & echo $! > "$out/proxy.pid"
  until curl -sf localhost:18300/v1/models >/dev/null; do sleep 1; done
  # Load all three before the first lane, so no lane pays a model load.
  for m in Qwen3.6-35B-A3B-MTP-UD-Q6_K Qwen3.5-4B-UD-MTP-Q6_K_XL; do
    curl -sf -m 600 localhost:18301/v1/chat/completions -H 'content-type: application/json' \
      -d "{\"model\":\"$m\",\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}],\"max_tokens\":4}" >/dev/null
  done
  curl -sf -m 600 localhost:18301/v1/embeddings -H 'content-type: application/json' \
    -d '{"model":"Qwen3-Embedding-0.6B-Q8_0","input":"hi"}' >/dev/null
  echo "arm: router up on :18301 with 3 models loaded, proxy on :18300"
}

write_config() {
  [ -f "$out/config.L.toml" ] || cp "$cfg" "$out/config.L.toml"
  if [ "$1" = L ]; then cp "$out/config.L.toml" "$cfg"; return; fi
  python3 - "$out/config.L.toml" "$cfg" <<'EOF'
import re, sys
src, dst = sys.argv[1], sys.argv[2]
text = open(src).read()
engine = '''[engine]
kind = "remote"
endpoint = "http://127.0.0.1:18300/v1"
model_id = "Qwen3.6-35B-A3B-MTP-UD-Q6_K"
fast_model_id = "Qwen3.5-4B-UD-MTP-Q6_K_XL"
embed_model_id = "Qwen3-Embedding-0.6B-Q8_0"
embed_inputs = "client"
context_size = 65536
'''
new, n = re.subn(r'(?ms)^\[engine\]\n.*?(?=^\[)', engine + "\n", text)
assert n == 1, "exactly one [engine] table"
open(dst, "w").write(new)
EOF
}

case ${1:-} in
  up)
    arm=${2:?L or R}
    daemon_down
    router_down
    write_config "$arm"
    [ "$arm" = R ] && router_up
    daemon_up "$arm"
    ;;
  check)
    run=${2:?run label, e.g. L1}
    for lane in $lanes; do
      t0=$(date +%s.%N)
      svrn_out=$(sovereign quality check --lane "$lane" 2>&1) || true
      t1=$(date +%s.%N)
      stamp=$(ls -t "$repo/target/quality-check" | head -1)
      printf '%s\t%s\t%s\t%s\t%s\n' "$run" "$lane" "$t0" "$t1" "$stamp" >> "$out/runs.tsv"
      printf '%s\n' "$svrn_out" > "$out/$run-$lane.out"
      echo "arm: $run $lane done in $(echo "$t1 - $t0" | bc | cut -d. -f1)s, stamp $stamp"
    done
    ;;
  restore)
    daemon_down
    router_down
    write_config L
    daemon_up L
    ;;
  *) echo "arm.sh up L|R | check <run> | restore" >&2; exit 2 ;;
esac
