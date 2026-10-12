#!/bin/sh
# Distribution spike D1-D3, loopback: llama-server --rpc to a local ggml-rpc-server.
S=$1; B=/home/alexbryan/dev/commonwealth-ai/target/llama-server/build-rpc-spike/bin
M=/home/alexbryan/dev/commonwealth-ai/sovereign/models/Qwen3.5-9B-UD-MTP-Q6_K_XL.gguf
RP=50152; P=48420; cache=$S/dist/cache; rm -rf $cache; mkdir -p $cache
LLAMA_CACHE=$cache $B/ggml-rpc-server -p $RP -c > $S/dist/rpc-server.log 2>&1 &
rpc=$!
trap 'kill -9 $rpc $srv 2>/dev/null; wait 2>/dev/null' EXIT
sleep 2
run() { # name, then llama-server args
  name=$1; shift
  t0=$(date +%s.%N)
  $B/llama-server -m $M --port $P -c 4096 -np 1 "$@" > $S/dist/$name.log 2>&1 &
  srv=$!
  ok=0
  for i in $(seq 1 600); do
    curl -sf -m 2 localhost:$P/health >/dev/null && { ok=1; break; }
    kill -0 $srv 2>/dev/null || break
    sleep 0.5
  done
  load=$(echo "$(date +%s.%N) - $t0" | bc)
  if [ $ok = 0 ]; then echo "$name: FAILED to load ($(tail -3 $S/dist/$name.log | tr '\n' ' ' | cut -c1-300))"; kill -9 $srv 2>/dev/null; wait $srv 2>/dev/null; return; fi
  rates=""
  for k in 1 2 3; do
    r=$(curl -s localhost:$P/completion -d '{"prompt":"Write a detailed history of the printing press.","n_predict":128,"temperature":0,"ignore_eos":true}' \
      | python3 -c "import json,sys; d=json.load(sys.stdin); t=d['timings']; print(round(t['predicted_per_second'],2), d['content'][:40].replace(chr(10),' '))")
    rates="$rates | $r"
  done
  echo "$name: load ${load}s $rates"
  kill $srv; wait $srv 2>/dev/null
}
run single     -ngl 99 -dev Vulkan0
run rpc-ts-cold -ngl 99 --rpc 127.0.0.1:$RP -dev RPC0,Vulkan0 -ts 1,1
run rpc-ts-warm -ngl 99 --rpc 127.0.0.1:$RP -dev RPC0,Vulkan0 -ts 1,1
run rpc-ot     -ngl 99 --rpc 127.0.0.1:$RP -dev RPC0,Vulkan0 -ot 'blk\.([0-9]|1[0-5])\.=RPC0'
echo "--- rpc-server cache: $(ls $cache/rpc 2>/dev/null | wc -l) files, $(du -sh $cache 2>/dev/null | cut -f1)"
grep -c "set_tensor_hash\|SET_TENSOR_HASH" $S/dist/rpc-server.log
tail -4 $S/dist/rpc-server.log
