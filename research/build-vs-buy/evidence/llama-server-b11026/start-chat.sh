#!/bin/sh
# usage: start-chat.sh <logfile>
D=$EVIDENCE
cd $D
nohup $D/mac/llama-b11026/llama-server -m ~/.svrnmesh/models/Qwen3.5-2B.Q6_K.gguf --host 127.0.0.1 --port 18431 -ngl 99 -c 8192 -np 2 --metrics --offline --reasoning off --slot-save-path $D/slots > $1 2>&1 &
echo $! > $D/pid-chat.txt
i=0; while [ $i -lt 60 ]; do if curl -s -m 2 http://127.0.0.1:18431/health | grep -q '"ok"'; then echo "ready pid=$(cat $D/pid-chat.txt)"; exit 0; fi; i=$((i+1)); sleep 1; done; echo "not ready"; exit 1
