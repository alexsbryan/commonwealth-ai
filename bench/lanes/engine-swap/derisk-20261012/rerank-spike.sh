#!/bin/sh
# Rerank spike R: the embedded rerank slot against llama-server --reranking,
# same GGUF, same 320 (query, document) pairs. Both servers are torn down on exit.
repo=/home/alexbryan/dev/commonwealth-ai
S=$1; root=$S/rerank/root; rm -rf $root; mkdir -p $root/home $root/rails $root/data
M=$repo/sovereign/models
RR=$M/qwen3-reranker-0.6b-q8_0.gguf
free_port() { python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])'; }
svrn=$(free_port); internal=$(free_port); serve=$(free_port); rails=$(free_port); lsp=$(free_port)
cat > $root/config.toml <<CFG
[engine]
kind = "llama"

[models]
primary = "$M/Qwen3.5-0.8B-UD-Q6_K_XL.gguf"
embed = "$M/Qwen3-Embedding-0.6B-Q8_0.gguf"
context_size = 4096

[models.kinds]
rerank = "$RR"

[daemon]
client_port = $svrn
internal_port = $internal
rails_base = "http://127.0.0.1:$rails"

[data]
dir = "$root/data"
CFG
HOME=$root/home SVRNMESH_DATA_DIR=$root/svrnmesh CW_RAILS_DIR=$root/rails CW_RAILS_BIN=$root/no-cw-rails \
  SOVEREIGN_SERVE_PORT=$serve $repo/target/debug/sovereign-stock run --config $root/config.toml > $S/rerank/daemon.log 2>&1 &
dpid=$!
$repo/target/debug/llama-server -m $RR --reranking --port $lsp -ngl 99 -c 8192 -b 8192 -ub 8192 -np 1 > $S/rerank/llama-server.log 2>&1 &
lpid=$!
trap 'kill -9 $dpid $lpid 2>/dev/null; wait 2>/dev/null' EXIT
for i in $(seq 1 300); do
  curl -sf -m 2 localhost:$lsp/health >/dev/null && curl -sf -m 2 localhost:$serve/v1/models >/dev/null && break
  kill -0 $dpid 2>/dev/null || { echo "daemon exited"; tail -20 $S/rerank/daemon.log; exit 1; }
  sleep 1
done
python3 $S/rerank-compare.py $S/rerank-pairs.json http://127.0.0.1:$serve http://127.0.0.1:$lsp $S/rerank
