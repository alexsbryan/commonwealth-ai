#!/bin/sh
S=$1; B=/home/alexbryan/dev/commonwealth-ai/target/llama-server/build-rpc-spike/bin
M=/home/alexbryan/dev/commonwealth-ai/sovereign/models/Qwen3.5-9B-UD-MTP-Q6_K_XL.gguf
RP=50152; P=48420; cache=$S/dist/cache
LLAMA_CACHE=$cache $B/ggml-rpc-server -p $RP -c > $S/dist/rpc-server-ot.log 2>&1 &
rpc=$!; trap 'kill -9 $rpc $srv 2>/dev/null; wait 2>/dev/null' EXIT; sleep 2
$B/llama-server -m $M --port $P -c 4096 -np 1 -ngl 99 --rpc 127.0.0.1:$RP -dev RPC0,Vulkan0 \
  -ot 'blk\.([0-9]|1[0-5])\.=RPC0[127.0.0.1:50152]' > $S/dist/rpc-ot.log 2>&1 &
srv=$!
for i in $(seq 1 600); do curl -sf -m 2 localhost:$P/health >/dev/null && break; kill -0 $srv 2>/dev/null || { echo "rpc-ot FAILED: $(tail -4 $S/dist/rpc-ot.log | tr '\n' ' ' | cut -c1-300)"; exit 1; }; sleep 0.5; done
for k in 1 2 3; do
  curl -s localhost:$P/completion -d '{"prompt":"Write a detailed history of the printing press.","n_predict":128,"temperature":0,"ignore_eos":true}' \
    | python3 -c "import json,sys; d=json.load(sys.stdin); print('rpc-ot', round(d['timings']['predicted_per_second'],2), d['content'][:40].replace(chr(10),' '))"
done
echo "loaded at $(grep 'model loaded' $S/dist/rpc-ot.log | cut -c1-12); rpc-server new tensors saved: $(grep -c 'saved to' $S/dist/rpc-server-ot.log)"
